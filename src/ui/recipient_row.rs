//! A recipient field of the composer (To, Cc, Bcc): every address is a pill,
//! and the one being typed is text after them. Space after a whole address,
//! Return or a comma turns the text into a pill, so the field never ends in
//! a comma. Backspace on an empty line turns the last pill back into text to
//! edit, until the next space, Return or comma.
//!
//! A pill drags to another place in its field or to another field, and
//! takes the keyboard (arrows from the start of the text, Delete, Return,
//! the menu key), whose menu moves it to another field too (#492).
//!
//! Built from libadwaita's own entry-row markup and class names (`row.entry`,
//! `.editable-area`, `.title`, `.subtitle`, `.edit-icon`, `.focused`), so the
//! row looks, focuses and turns red on an error the way the Subject row under
//! it does.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use gtk::glib::subclass::Signal;

use crate::i18n::{i18n, i18n_f};

/// Space between pills, and between lines of them.
const SPACING: i32 = 4;
/// The narrowest the text after the pills gets before it takes a line of
/// its own.
const TEXT_MIN: i32 = 96;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RecipientRow {
        pub header: gtk::Box,
        pub empty_title: gtk::Label,
        pub title: gtk::Label,
        pub flow: super::PillFlow,
        pub text: gtk::Text,
        pub edit_icon: gtk::Image,
        pub suffixes: gtk::Box,
        /// The pills in order, each with the recipient it stands for as it
        /// goes into the field ("Name <address>").
        pub pills: RefCell<Vec<(gtk::Widget, String)>>,
        /// Set while the row rewrites its own text, so that is not taken
        /// for typing.
        pub busy: Cell<bool>,
        pub focused: Cell<bool>,
        /// The other recipient fields of the composer, which a pill's menu
        /// offers to move it to.
        pub peers: RefCell<Vec<glib::WeakRef<super::RecipientRow>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecipientRow {
        const NAME: &'static str = "HylkiRecipientRow";
        type Type = super::RecipientRow;
        type ParentType = adw::PreferencesRow;
    }

    impl ObjectImpl for RecipientRow {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("changed").build(),
                    // A pill of this row starts (true) or ends (false) a drag.
                    Signal::builder("pill-drag").param_types([bool::static_type()]).build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_css_class("entry");
            obj.add_css_class("recipient-row");
            obj.set_activatable(false);
            // The text takes the focus, never the row (see `focus`).
            obj.set_focusable(false);

            self.header.add_css_class("header");
            self.header.set_valign(gtk::Align::Center);

            let area = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            area.add_css_class("editable-area");
            area.set_hexpand(true);

            for label in [&self.empty_title, &self.title] {
                label.set_halign(gtk::Align::Start);
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_can_target(false);
                label.set_use_markup(false);
                obj.bind_property("title", label, "label").sync_create().build();
            }
            self.empty_title.add_css_class("title");
            self.empty_title.add_css_class("dimmed");
            self.empty_title.set_valign(gtk::Align::Center);
            self.title.add_css_class("subtitle");

            self.text.set_hexpand(true);
            self.text.set_valign(gtk::Align::Center);
            self.text.set_enable_undo(true);
            self.text.set_accessible_role(gtk::AccessibleRole::TextBox);
            self.text.update_relation(&[gtk::accessible::Relation::LabelledBy(&[self.title.upcast_ref::<gtk::Accessible>()])]);
            self.flow.add_css_class("recipient-flow");
            self.flow.set_text_child(&self.text);

            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.set_hexpand(true);
            column.set_valign(gtk::Align::Center);
            column.append(&self.title);
            column.append(&self.flow);
            // The title sits where the text would be until there is some, as
            // in an entry row; it overlays the column so the row keeps its
            // height either way.
            let overlay = gtk::Overlay::new();
            overlay.set_hexpand(true);
            overlay.set_child(Some(&column));
            overlay.add_overlay(&self.empty_title);
            area.append(&overlay);

            self.edit_icon.set_icon_name(Some("adw-entry-edit-symbolic"));
            self.edit_icon.add_css_class("edit-icon");
            self.edit_icon.set_valign(gtk::Align::Center);
            self.edit_icon.set_can_target(false);
            self.edit_icon.set_accessible_role(gtk::AccessibleRole::Presentation);
            area.append(&self.edit_icon);
            obj.wire_focus(&area);

            self.header.append(&area);
            self.suffixes.add_css_class("suffixes");
            self.suffixes.set_spacing(6);
            self.suffixes.set_visible(false);
            self.header.append(&self.suffixes);
            obj.set_child(Some(&self.header));

            obj.wire();
            obj.update_state();
        }
    }

    impl WidgetImpl for RecipientRow {
        fn grab_focus(&self) -> bool {
            self.text.grab_focus()
        }

        fn focus(&self, direction: gtk::DirectionType) -> bool {
            self.header.child_focus(direction)
        }
    }

    impl ListBoxRowImpl for RecipientRow {}
    impl PreferencesRowImpl for RecipientRow {}
}

glib::wrapper! {
    pub struct RecipientRow(ObjectSubclass<imp::RecipientRow>)
        @extends adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for RecipientRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl RecipientRow {
    /// The whole field as one recipient list: the pills, then whatever is
    /// still being typed, comma separated.
    pub fn text(&self) -> String {
        let imp = self.imp();
        let mut all: Vec<String> = imp.pills.borrow().iter().map(|(_, r)| r.clone()).collect();
        let typed = self.typed();
        if !typed.is_empty() {
            all.push(typed);
        }
        all.join(", ")
    }

    /// Replace the field with these recipients, every one a pill.
    pub fn set_text(&self, field: &str) {
        let imp = self.imp();
        for (pill, _) in imp.pills.take() {
            imp.flow.remove(&pill);
        }
        self.set_typed("");
        self.push(field);
        self.emit_changed();
    }

    /// What is being typed after the pills, which suggestions are for.
    pub fn typed(&self) -> String {
        self.imp().text.text().trim().to_string()
    }

    /// Put a recipient in place of what is being typed (a suggestion
    /// picked from the list).
    pub fn complete(&self, recipient: &str) {
        self.set_typed("");
        self.push(recipient);
        self.emit_changed();
    }

    /// Add a recipient after the others, leaving what is being typed alone.
    pub fn add_recipient(&self, recipient: &str) {
        self.push(recipient);
        self.emit_changed();
    }

    pub fn add_suffix(&self, widget: &impl IsA<gtk::Widget>) {
        let imp = self.imp();
        imp.suffixes.append(widget);
        imp.suffixes.set_visible(true);
    }

    pub fn set_input_purpose(&self, purpose: gtk::InputPurpose) {
        self.imp().text.set_input_purpose(purpose);
    }

    /// Called after every change to the field: typing, a pill made or
    /// taken away.
    pub fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("changed", false, move |values| {
            let row = values[0].get::<Self>().expect("RecipientRow");
            f(&row);
            None
        })
    }

    fn emit_changed(&self) {
        self.update_state();
        self.emit_by_name::<()>("changed", &[]);
    }

    fn wire(&self) {
        let imp = self.imp();
        let text = &imp.text;

        // Keys are not enough: an input method or a paste inserts text
        // without them, so separators are caught as the text goes in.
        let weak = self.downgrade();
        text.connect_insert_text(move |text, new, pos| {
            let Some(row) = weak.upgrade() else { return };
            if row.imp().busy.get() {
                return;
            }
            let current = text.text().to_string();
            let at = current.char_indices().nth((*pos).max(0) as usize).map_or(current.len(), |(i, _)| i);
            let (before, after) = current.split_at(at);
            if let Some(action) = on_insert(before, new, after) {
                text.stop_signal_emission_by_name("insert-text");
                match action {
                    Insert::Swallow => {}
                    Insert::Commit { done, rest } => {
                        row.push(&done);
                        row.set_typed(&rest);
                        row.emit_changed();
                    }
                }
            }
        });

        let weak = self.downgrade();
        text.connect_changed(move |_| {
            let Some(row) = weak.upgrade() else { return };
            if !row.imp().busy.get() {
                row.emit_changed();
            }
        });

        // Return makes a pill of whatever was typed. The composer catches
        // Return first while its suggestion list is open, to pick one.
        let weak = self.downgrade();
        text.connect_activate(move |_| {
            let Some(row) = weak.upgrade() else { return };
            let typed = row.typed();
            if !typed.is_empty() {
                row.complete(&typed);
            }
        });

        // Backspace on an empty line: the last pill becomes text again.
        let weak = self.downgrade();
        text.connect_backspace(move |text| {
            let Some(row) = weak.upgrade() else { return };
            if !text.text().is_empty() {
                return;
            }
            let Some((pill, recipient)) = row.imp().pills.borrow_mut().pop() else { return };
            text.stop_signal_emission_by_name("backspace");
            row.imp().flow.remove(&pill);
            row.set_typed(&recipient);
            row.emit_changed();
        });

        // Left from the start of the text goes to the last pill, to move
        // through them with the keyboard.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(row) = weak.upgrade() else { return glib::Propagation::Proceed };
            let text = &row.imp().text;
            let towards = if row.direction() == gtk::TextDirection::Rtl { gdk::Key::Right } else { gdk::Key::Left };
            let plain = !state.intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
            if key != towards || !plain || text.position() != 0 || text.selection_bounds().is_some() {
                return glib::Propagation::Proceed;
            }
            let last = row.imp().pills.borrow().last().map(|(p, _)| p.clone());
            match last {
                Some(pill) => {
                    pill.grab_focus();
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        text.add_controller(keys);

        // A click anywhere on the row, pills aside, types into it.
        let click = gtk::GestureClick::new();
        let weak = self.downgrade();
        click.connect_released(move |gesture, _, x, y| {
            let Some(row) = weak.upgrade() else { return };
            let header = gesture.widget();
            let on_pill = header
                .and_then(|h| h.pick(x, y, gtk::PickFlags::DEFAULT))
                .is_some_and(|w| is_in_pill(&w));
            if !on_pill {
                row.imp().text.grab_focus();
            }
        });
        imp.header.add_controller(click);

        // A pill dragged here, from this field or another. In the capture
        // phase, ahead of the text's own target, which would take the pill
        // as typed text.
        let target = gtk::DropTarget::new(gtk::Widget::static_type(), gdk::DragAction::MOVE);
        target.set_propagation_phase(gtk::PropagationPhase::Capture);
        target.set_preload(true);
        let weak = self.downgrade();
        target.connect_accept(move |_, drop| {
            weak.upgrade().is_some()
                && drop.formats().contains_type(gtk::Widget::static_type())
                && drop.actions().contains(gdk::DragAction::MOVE)
        });
        let weak = self.downgrade();
        target.connect_motion(move |target, x, y| {
            let Some(row) = weak.upgrade() else { return gdk::DragAction::empty() };
            let dragged = target.value().and_then(|v| v.get::<gtk::Widget>().ok());
            let at = row.drop_index(x, y);
            // No marker where the pill already is, since letting go there
            // leaves it as it was, nor in a field with no pills, where the
            // field's own highlight says enough.
            let stays = dragged.as_ref().and_then(|p| row.index_of(p)).is_some_and(|i| at == i || at == i + 1);
            let empty = row.imp().pills.borrow().is_empty();
            row.imp().flow.set_marker((!stays && !empty).then_some(at));
            gdk::DragAction::MOVE
        });
        let weak = self.downgrade();
        target.connect_leave(move |_| {
            if let Some(row) = weak.upgrade() {
                row.imp().flow.set_marker(None);
            }
        });
        let weak = self.downgrade();
        target.connect_drop(move |_, value, x, y| {
            let Some(row) = weak.upgrade() else { return false };
            row.imp().flow.set_marker(None);
            let Ok(pill) = value.get::<gtk::Widget>() else { return false };
            let Some(from) = pill.ancestor(Self::static_type()).and_downcast::<Self>() else { return false };
            from.move_pill(&pill, &row, row.drop_index(x, y));
            true
        });
        self.add_controller(target);
    }

    /// The field looks focused while its text or one of its pills has the
    /// focus, as an entry row does while its text has.
    fn wire_focus(&self, area: &gtk::Box) {
        let focus = gtk::EventControllerFocus::new();
        let weak = self.downgrade();
        focus.connect_enter(move |_| {
            if let Some(row) = weak.upgrade() {
                row.imp().focused.set(true);
                row.update_state();
            }
        });
        let weak = self.downgrade();
        focus.connect_leave(move |_| {
            let Some(row) = weak.upgrade() else { return };
            row.imp().focused.set(false);
            row.update_state();
            // A whole address left behind becomes a pill like the rest, once
            // the focus has gone to another field (a pill of this one keeps
            // it here). Not when the window loses it: someone who switched
            // away to look the rest of an address up comes back to the text
            // as they left it.
            let weak = row.downgrade();
            glib::idle_add_local_once(move || {
                let Some(row) = weak.upgrade() else { return };
                let active = row.root().and_downcast::<gtk::Window>().is_some_and(|w| w.is_active());
                let typed = row.typed();
                if active && !row.imp().focused.get() && is_whole_address(&typed) {
                    row.complete(&typed);
                }
            });
        });
        area.add_controller(focus);
    }

    /// Make pills of every recipient in `field`, after the others.
    fn push(&self, field: &str) {
        for (name, addr) in crate::worker::parse_recipients(&normalise_separators(field)) {
            if addr.is_empty() {
                continue;
            }
            let recipient = crate::worker::format_recipient(&name, &addr);
            let at = self.imp().pills.borrow().len();
            self.insert_pill(at, &name, &addr, &recipient);
        }
    }

    /// Make a pill of one recipient and put it `at` that place among the
    /// others.
    fn insert_pill(&self, at: usize, name: &str, addr: &str, recipient: &str) -> gtk::Widget {
        let imp = self.imp();
        let pill = self.pill(name, addr, recipient).upcast::<gtk::Widget>();
        let next = imp.pills.borrow().get(at).map(|(p, _)| p.clone());
        imp.flow.place(&pill, next.as_ref());
        let mut pills = imp.pills.borrow_mut();
        let at = at.min(pills.len());
        pills.insert(at, (pill.clone(), recipient.to_string()));
        pill
    }

    fn pill(&self, name: &str, addr: &str, recipient: &str) -> gtk::Box {
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("recipient-pill");
        // Focusable for the keyboard (see `pill_key`), but Tab passes over
        // the pills (see the flow's `focus`).
        pill.set_focusable(true);
        let valid = crate::worker::invalid_recipient(recipient).is_none();
        if !valid {
            pill.add_css_class("invalid");
        }
        let shown = if name.is_empty() { addr } else { name };
        pill.update_property(&[gtk::accessible::Property::Label(shown)]);
        let label = gtk::Label::new(Some(shown));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(32);
        label.set_single_line_mode(true);
        pill.append(&label);
        pill.set_tooltip_text(Some(if valid {
            addr.to_string()
        } else {
            i18n_f("“{addr}” is not an email address.", &[("addr", addr)])
        }
        .as_str()));

        let remove = gtk::Button::from_icon_name("window-close-symbolic");
        remove.add_css_class("flat");
        remove.add_css_class("circular");
        remove.set_focusable(false);
        remove.set_focus_on_click(false);
        remove.set_valign(gtk::Align::Center);
        remove.set_tooltip_text(Some(i18n("Remove").as_str()));
        remove.update_property(&[gtk::accessible::Property::Label(&i18n_f("Remove {name}", &[("name", shown)]))]);
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        remove.connect_clicked(move |_| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else { return };
            row.remove_pill(pill.upcast_ref());
        });
        pill.append(&remove);

        // A click selects the pill, for the keyboard; a double click edits
        // it: it becomes the text, and what was being typed is kept as a
        // pill of its own first.
        let click = gtk::GestureClick::new();
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        click.connect_pressed(move |gesture, n, x, y| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else { return };
            let on_remove = pill.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| w.ancestor(gtk::Button::static_type()).is_some());
            if on_remove {
                return;
            }
            if n == 2 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                row.edit_pill(pill.upcast_ref());
            } else {
                pill.grab_focus();
            }
        });
        pill.add_controller(click);

        let menu = gtk::GestureClick::new();
        menu.set_button(gdk::BUTTON_SECONDARY);
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        menu.connect_pressed(move |gesture, _, x, y| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else { return };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            pill.grab_focus();
            row.pill_menu(pill.upcast_ref(), x, y);
        });
        pill.add_controller(menu);

        let keys = gtk::EventControllerKey::new();
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            row.pill_key(pill.upcast_ref(), key, state)
        });
        pill.add_controller(keys);

        // Dragged to another place in this field or into another. The
        // drag carries the pill itself, which only a recipient field takes.
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        let weak_pill = pill.downgrade();
        drag.connect_prepare(move |source, x, y| {
            let pill = weak_pill.upgrade()?;
            // The pill as it looks now, before it dims for the drag.
            let image = gtk::WidgetPaintable::new(Some(&pill)).current_image();
            source.set_icon(Some(&image), x as i32, y as i32);
            Some(gdk::ContentProvider::for_value(&pill.upcast::<gtk::Widget>().to_value()))
        });
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        drag.connect_drag_begin(move |_, _| {
            if let Some(pill) = weak_pill.upgrade() {
                pill.add_css_class("dragged");
            }
            if let Some(row) = weak.upgrade() {
                row.emit_by_name::<()>("pill-drag", &[&true]);
            }
        });
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(pill) = weak_pill.upgrade() {
                pill.remove_css_class("dragged");
            }
            if let Some(row) = weak.upgrade() {
                row.emit_by_name::<()>("pill-drag", &[&false]);
            }
        });
        pill.add_controller(drag);
        pill
    }

    /// The other recipient fields, for a pill's menu to move it to.
    pub fn set_peers(&self, peers: &[&RecipientRow]) {
        *self.imp().peers.borrow_mut() = peers.iter().filter(|p| **p != self).map(|p| p.downgrade()).collect();
    }

    /// Called as a pill of this field starts (true) and ends (false) a
    /// drag.
    pub fn connect_pill_drag<F: Fn(&Self, bool) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("pill-drag", false, move |values| {
            let row = values[0].get::<Self>().expect("RecipientRow");
            f(&row, values[1].get::<bool>().unwrap_or(false));
            None
        })
    }

    fn index_of(&self, pill: &gtk::Widget) -> Option<usize> {
        self.imp().pills.borrow().iter().position(|(p, _)| p == pill)
    }

    /// Where among the pills a pill let go at `(x, y)` goes: before the
    /// first pill on the pointer's line that is past it, after those on
    /// the lines above.
    fn drop_index(&self, x: f64, y: f64) -> usize {
        let imp = self.imp();
        let flow = &imp.flow;
        let Some(p) = self.compute_point(flow, &gtk::graphene::Point::new(x as f32, y as f32)) else { return 0 };
        let rtl = flow.direction() == gtk::TextDirection::Rtl;
        let y = (p.y() as f64).clamp(0.0, (flow.height() - 1).max(0) as f64);
        imp.pills
            .borrow()
            .iter()
            .filter_map(|(pill, _)| pill.compute_bounds(flow))
            .filter(|b| {
                let (top, bottom) = (b.y() as f64, (b.y() + b.height()) as f64);
                let middle = (b.x() + b.width() / 2.0) as f64;
                let past = if rtl { (p.x() as f64) > middle } else { (p.x() as f64) < middle };
                bottom <= y || (top <= y && !past)
            })
            .count()
    }

    /// Move `pill` of this field to place `at` in `to`, which may be this
    /// field again. A field that already has the address only loses it
    /// here. Returns the pill in its new place.
    fn move_pill(&self, pill: &gtk::Widget, to: &RecipientRow, at: usize) -> Option<gtk::Widget> {
        let imp = self.imp();
        let from = self.index_of(pill)?;
        let focused = pill.is_focus();
        if to == self {
            if at == from || at == from + 1 {
                return Some(pill.clone());
            }
            let entry = imp.pills.borrow_mut().remove(from);
            let at = if at > from { at - 1 } else { at };
            let next = imp.pills.borrow().get(at).map(|(p, _)| p.clone());
            imp.flow.place(&entry.0, next.as_ref());
            imp.pills.borrow_mut().insert(at, entry);
            self.emit_changed();
            return Some(pill.clone());
        }
        let recipient = imp.pills.borrow()[from].1.clone();
        let (name, addr) = crate::worker::parse_recipients(&recipient).into_iter().next()?;
        imp.pills.borrow_mut().remove(from);
        imp.flow.remove(pill);
        self.emit_changed();
        let there = to.imp().pills.borrow().iter().find_map(|(p, r)| {
            let other = crate::worker::parse_recipients(r).into_iter().next().map(|(_, a)| a);
            other.is_some_and(|a| a.eq_ignore_ascii_case(&addr)).then(|| p.clone())
        });
        let moved = match there {
            Some(p) => p,
            None => {
                let p = to.insert_pill(at, &name, &addr, &recipient);
                to.emit_changed();
                p
            }
        };
        if focused {
            moved.grab_focus();
        }
        Some(moved)
    }

    /// Keys on a focused pill: arrows between pills and back to the text,
    /// Delete or Backspace removes it, Return edits it, the menu key opens
    /// its menu, and a letter goes to the text.
    fn pill_key(&self, pill: &gtk::Widget, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let imp = self.imp();
        let Some(i) = self.index_of(pill) else { return glib::Propagation::Proceed };
        let rtl = self.direction() == gtk::TextDirection::Rtl;
        let pill_at = |j: usize| imp.pills.borrow().get(j).map(|(p, _)| p.clone());
        let modified = state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
        let (back, forward) = if rtl { (gdk::Key::Right, gdk::Key::Left) } else { (gdk::Key::Left, gdk::Key::Right) };
        match key {
            _ if modified => return glib::Propagation::Proceed,
            k if k == back => {
                if let Some(p) = i.checked_sub(1).and_then(pill_at) {
                    p.grab_focus();
                }
            }
            k if k == forward => match pill_at(i + 1) {
                Some(p) => {
                    p.grab_focus();
                }
                None => self.focus_text(),
            },
            gdk::Key::Home => {
                if let Some(p) = pill_at(0) {
                    p.grab_focus();
                }
            }
            gdk::Key::End => self.focus_text(),
            gdk::Key::BackSpace | gdk::Key::Delete | gdk::Key::KP_Delete => {
                // Backspace leaves the focus on the pill before, Delete on
                // the one after, as they would on letters.
                let next = if key == gdk::Key::BackSpace && i > 0 { pill_at(i - 1) } else { pill_at(i + 1) };
                let next = next.or_else(|| i.checked_sub(1).and_then(pill_at));
                self.remove_pill(pill);
                match next {
                    Some(p) => {
                        p.grab_focus();
                    }
                    None => self.focus_text(),
                }
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter | gdk::Key::F2 => self.edit_pill(pill),
            gdk::Key::Menu => self.pill_menu(pill, pill.width() as f64 / 2.0, pill.height() as f64),
            gdk::Key::F10 if state.contains(gdk::ModifierType::SHIFT_MASK) => {
                self.pill_menu(pill, pill.width() as f64 / 2.0, pill.height() as f64)
            }
            // Typing goes on in the text, after the pills.
            _ => match key.to_unicode().filter(|c| !c.is_control()) {
                Some(c) => {
                    self.focus_text();
                    let text = &imp.text;
                    let mut end = text.text().chars().count() as i32;
                    text.insert_text(&c.to_string(), &mut end);
                    text.set_position(end);
                }
                None => return glib::Propagation::Proceed,
            },
        }
        glib::Propagation::Stop
    }

    /// The pill's menu, at `(x, y)` in the pill: move it to another field,
    /// edit it, remove it.
    fn pill_menu(&self, pill: &gtk::Widget, x: f64, y: f64) {
        use crate::ui::context_menu::{show_context_menu, MenuEntry};
        let Some(p) = pill.compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32)) else { return };
        let mut moves = Vec::new();
        for peer in self.imp().peers.borrow().iter().filter_map(|p| p.upgrade()) {
            let label = i18n_f("Move to {name}", &[("name", &peer.title())]);
            let (row, pill) = (self.downgrade(), pill.downgrade());
            moves.push(MenuEntry::new(label, move || {
                let (Some(row), Some(pill)) = (row.upgrade(), pill.upgrade()) else { return };
                peer.set_visible(true);
                let at = peer.imp().pills.borrow().len();
                if let Some(moved) = row.move_pill(&pill, &peer, at) {
                    moved.grab_focus();
                }
            }));
        }
        let (row, weak) = (self.downgrade(), pill.downgrade());
        let edit = MenuEntry::new(i18n("Edit"), move || {
            if let (Some(row), Some(pill)) = (row.upgrade(), weak.upgrade()) {
                row.edit_pill(&pill);
            }
        });
        let (row, weak) = (self.downgrade(), pill.downgrade());
        let remove = MenuEntry::new(i18n("Remove"), move || {
            if let (Some(row), Some(pill)) = (row.upgrade(), weak.upgrade()) {
                row.remove_pill(&pill);
                row.focus_text();
            }
        });
        show_context_menu(self, p.x() as f64, p.y() as f64, vec![moves, vec![edit, remove]]);
    }

    fn focus_text(&self) {
        let text = &self.imp().text;
        text.grab_focus();
        text.set_position(-1);
    }

    fn remove_pill(&self, pill: &gtk::Widget) {
        let imp = self.imp();
        imp.pills.borrow_mut().retain(|(p, _)| p != pill);
        imp.flow.remove(pill);
        self.emit_changed();
    }

    fn edit_pill(&self, pill: &gtk::Widget) {
        let imp = self.imp();
        let typed = self.typed();
        if !typed.is_empty() {
            self.push(&typed);
        }
        let recipient = {
            let mut pills = imp.pills.borrow_mut();
            let Some(i) = pills.iter().position(|(p, _)| p == pill) else { return };
            pills.remove(i).1
        };
        imp.flow.remove(pill);
        self.set_typed(&recipient);
        imp.text.grab_focus();
        imp.text.set_position(-1);
        self.emit_changed();
    }

    /// Set the typed text without it counting as typing.
    fn set_typed(&self, s: &str) {
        let imp = self.imp();
        imp.busy.set(true);
        imp.text.set_text(s.trim_start());
        imp.text.set_position(-1);
        imp.busy.set(false);
    }

    /// The floating title, the focus ring and the pencil, as an entry row
    /// shows them.
    fn update_state(&self) {
        let imp = self.imp();
        let focused = imp.focused.get();
        let empty = imp.pills.borrow().is_empty() && imp.text.text().is_empty();
        let floating = focused || !empty;
        imp.empty_title.set_visible(!floating);
        imp.title.set_opacity(if floating { 1.0 } else { 0.0 });
        imp.edit_icon.set_visible(!focused);
        if focused {
            self.add_css_class("focused");
        } else {
            self.remove_css_class("focused");
        }
    }
}

/// Whether `widget` is a pill or inside one.
fn is_in_pill(widget: &gtk::Widget) -> bool {
    let mut w = Some(widget.clone());
    while let Some(c) = w {
        if c.has_css_class("recipient-pill") {
            return true;
        }
        w = c.parent();
    }
    false
}

/// What to do with text about to go into the field, when it is more than
/// plain typing.
#[derive(Debug, PartialEq)]
enum Insert {
    /// Leave it out: a space before anything is typed, or a lone comma.
    Swallow,
    /// Make pills of `done` and leave `rest` as the text.
    Commit { done: String, rest: String },
}

/// Decide what inserting `new` between `before` and `after` does. `None`
/// lets it go in as typed.
fn on_insert(before: &str, new: &str, after: &str) -> Option<Insert> {
    let joined = format!("{before}{new}{after}");
    // A separator outside quotes and angle brackets ends a recipient: typed
    // or pasted, everything up to the last one becomes pills.
    if let Some(cut) = last_separator(&joined) {
        let (done, rest) = joined.split_at(cut);
        let rest = rest[1..].trim_start().to_string();
        if done.trim().is_empty() {
            return Some(if rest.is_empty() {
                Insert::Swallow
            } else {
                Insert::Commit { done: String::new(), rest }
            });
        }
        return Some(Insert::Commit { done: done.to_string(), rest });
    }
    if new.chars().all(char::is_whitespace) {
        // Nothing to separate yet.
        if before.trim().is_empty() && after.trim().is_empty() {
            return Some(Insert::Swallow);
        }
        // A space at the end of a whole address finishes it; anywhere else,
        // or after a name, it is part of what is being typed.
        if after.trim().is_empty() && is_whole_address(before) {
            return Some(Insert::Commit { done: before.to_string(), rest: String::new() });
        }
        return None;
    }
    // Several addresses pasted with spaces between them.
    if new.chars().any(char::is_whitespace) {
        let words: Vec<&str> = joined.split_whitespace().collect();
        let ended = words.len() > 1 || (after.trim().is_empty() && new.ends_with(char::is_whitespace));
        if ended && !words.is_empty() && words.iter().all(|w| is_whole_address(w)) {
            return Some(Insert::Commit { done: words.join(", "), rest: String::new() });
        }
    }
    None
}

/// The byte index of the last comma, semicolon or line break that separates
/// two recipients, outside a quoted name and an address's angle brackets.
fn last_separator(s: &str) -> Option<usize> {
    let (mut quoted, mut escaped, mut angle) = (false, false, false);
    let mut last = None;
    for (i, ch) in s.char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' | '\n' | '\r' if !quoted && !angle => last = Some(i),
            _ => {}
        }
    }
    last
}

/// Outlook's semicolons and pasted line breaks read as commas.
fn normalise_separators(s: &str) -> String {
    let (mut quoted, mut escaped, mut angle) = (false, false, false);
    s.chars()
        .map(|ch| {
            match ch {
                _ if escaped => escaped = false,
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                '<' if !quoted => angle = true,
                '>' if !quoted => angle = false,
                ';' | '\n' | '\r' if !quoted && !angle => return ',',
                _ => {}
            }
            ch
        })
        .collect()
}

/// Whether the text is one complete recipient that can be sent to: an
/// address, or a name with its address closed in angle brackets.
fn is_whole_address(s: &str) -> bool {
    let s = s.trim();
    if !s.contains('@') || (s.contains('<') && !s.ends_with('>')) {
        return false;
    }
    if s.chars().filter(|&c| c == '"').count() % 2 == 1 {
        return false;
    }
    crate::worker::parse_recipients(s).len() == 1 && crate::worker::invalid_recipient(s).is_none()
}

mod flow_imp {
    use super::*;

    /// Lays the pills out left to right, wrapping, with the text after the
    /// last one taking what is left of its line.
    #[derive(Default)]
    pub struct PillFlow {
        pub text: RefCell<Option<gtk::Widget>>,
        /// The line showing where a dragged pill would go, outside the
        /// layout.
        pub marker: gtk::Box,
        /// The pill the marker stands before; the number of pills puts it
        /// after the last.
        pub marker_at: Cell<Option<usize>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PillFlow {
        const NAME: &'static str = "HylkiPillFlow";
        type Type = super::PillFlow;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PillFlow {
        fn constructed(&self) {
            self.parent_constructed();
            self.marker.add_css_class("pill-drop-marker");
            self.marker.set_can_target(false);
            self.marker.set_visible(false);
            self.marker.set_parent(&*self.obj());
        }

        fn dispose(&self) {
            while let Some(c) = self.obj().first_child() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for PillFlow {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let obj = self.obj();
            if orientation == gtk::Orientation::Horizontal {
                let mut min = TEXT_MIN;
                let mut nat = 0;
                for child in obj.children() {
                    let (cmin, cnat, _, _) = child.measure(orientation, -1);
                    min = min.max(cmin);
                    nat += cnat.max(if obj.is_text(&child) { TEXT_MIN } else { 0 }) + SPACING;
                }
                (min, (nat - SPACING).max(min), -1, -1)
            } else {
                let width = if for_size < 0 { i32::MAX / 2 } else { for_size };
                let lines = obj.lines(width);
                let h = lines.last().map_or(0, |l| l.y + l.h);
                // Wrapped lines get room under the last one, which a single
                // line has from the row's own height.
                let h = if lines.len() > 1 { h + SPACING } else { h };
                (h, h, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let obj = self.obj();
            let rtl = obj.direction() == gtk::TextDirection::Rtl;
            // Each child's place before mirroring, for the marker.
            let mut placed = Vec::new();
            for line in obj.lines(width) {
                for (child, x, w, h) in line.items {
                    let y = line.y + (line.h - h) / 2;
                    placed.push((obj.is_text(&child), x, y, w, h));
                    let x = if rtl { width - x - w } else { x };
                    child.size_allocate(&gtk::Allocation::new(x, y, w, h), -1);
                }
            }
            if let Some(at) = self.marker_at.get() {
                let pills: Vec<_> = placed.iter().filter(|p| !p.0).collect();
                let (w, _, _, _) = self.marker.measure(gtk::Orientation::Horizontal, -1);
                let w = w.max(2);
                // In the gap before the pill, or after the last one, or at
                // the start of the text when there is none.
                let (x, y, h) = match (pills.get(at), pills.last(), placed.iter().find(|p| p.0)) {
                    (Some(p), _, _) => (p.1 - (SPACING + w) / 2, p.2, p.4),
                    (None, Some(p), _) => (p.1 + p.3 + (SPACING - w) / 2, p.2, p.4),
                    (None, None, Some(t)) => (t.1, t.2, t.4),
                    _ => (0, 0, 0),
                };
                let x = x.clamp(0, (width - w).max(0));
                let x = if rtl { width - x - w } else { x };
                self.marker.size_allocate(&gtk::Allocation::new(x, y, w, h), -1);
            }
        }

        /// Tab goes to the text and on out of the field, never through the
        /// pills, which the arrow keys reach from the text.
        fn focus(&self, direction: gtk::DirectionType) -> bool {
            match direction {
                gtk::DirectionType::TabForward | gtk::DirectionType::TabBackward => {
                    if self.obj().focus_child().is_some() {
                        return false;
                    }
                    self.text.borrow().as_ref().is_some_and(|t| t.grab_focus())
                }
                _ => self.parent_focus(direction),
            }
        }
    }
}

glib::wrapper! {
    pub struct PillFlow(ObjectSubclass<flow_imp::PillFlow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PillFlow {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// One line of the flow: each child with its x, width and height.
struct Line {
    y: i32,
    h: i32,
    items: Vec<(gtk::Widget, i32, i32, i32)>,
}

impl PillFlow {
    /// The text, always the last child.
    fn set_text_child(&self, text: &impl IsA<gtk::Widget>) {
        text.set_parent(self);
        self.imp().text.replace(Some(text.clone().upcast()));
    }

    /// Put a pill before `next`, or after the others; a pill already here
    /// moves.
    fn place(&self, pill: &impl IsA<gtk::Widget>, next: Option<&gtk::Widget>) {
        let text = self.imp().text.borrow().clone();
        pill.insert_before(self, next.or(text.as_ref()));
        self.queue_resize();
    }

    /// Show the drop marker before pill `at`, or hide it.
    fn set_marker(&self, at: Option<usize>) {
        let imp = self.imp();
        if imp.marker_at.get() == at {
            return;
        }
        imp.marker_at.set(at);
        imp.marker.set_visible(at.is_some());
        self.queue_allocate();
    }

    fn remove(&self, child: &impl IsA<gtk::Widget>) {
        if child.parent().as_ref() == Some(self.upcast_ref()) {
            child.unparent();
            self.queue_resize();
        }
    }

    fn is_text(&self, child: &gtk::Widget) -> bool {
        self.imp().text.borrow().as_ref() == Some(child)
    }

    fn children(&self) -> Vec<gtk::Widget> {
        let mut out = Vec::new();
        let mut child = self.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if c.is_visible() && c != *self.imp().marker.upcast_ref::<gtk::Widget>() {
                out.push(c);
            }
        }
        out
    }

    fn lines(&self, width: i32) -> Vec<Line> {
        let mut lines: Vec<Line> = Vec::new();
        let mut cur = Line { y: 0, h: 0, items: Vec::new() };
        let mut x = 0;
        for child in self.children() {
            let (_, nat, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            let text = self.is_text(&child);
            // The text needs room to type in; a pill as much as its name,
            // cut short at the full width.
            let need = if text { TEXT_MIN } else { nat.min(width) };
            if x > 0 && x + need > width {
                let y = cur.y + cur.h + SPACING;
                lines.push(std::mem::replace(&mut cur, Line { y, h: 0, items: Vec::new() }));
                x = 0;
            }
            let w = if text { (width - x).max(need) } else { need };
            let (_, h, _, _) = child.measure(gtk::Orientation::Vertical, w);
            cur.h = cur.h.max(h);
            cur.items.push((child, x, w, h));
            x += w + SPACING;
        }
        lines.push(cur);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(done: &str, rest: &str) -> Option<Insert> {
        Some(Insert::Commit { done: done.into(), rest: rest.into() })
    }

    #[test]
    fn a_space_after_a_whole_address_makes_a_pill() {
        assert_eq!(on_insert("ann@example.com", " ", ""), commit("ann@example.com", ""));
        assert_eq!(on_insert("Ann Lee <ann@example.com>", " ", ""), commit("Ann Lee <ann@example.com>", ""));
    }

    #[test]
    fn a_space_in_a_name_or_a_half_address_is_typed() {
        assert_eq!(on_insert("Ann", " ", ""), None);
        assert_eq!(on_insert("Ann <ann@exa", " ", ""), None);
        assert_eq!(on_insert("\"Lee", " ", ""), None);
        // Inside the text rather than at its end.
        assert_eq!(on_insert("ann@example.com", " ", "x"), None);
    }

    #[test]
    fn a_comma_makes_a_pill_and_never_stays() {
        assert_eq!(on_insert("ann@example.com", ",", ""), commit("ann@example.com", ""));
        assert_eq!(on_insert("Ann", ",", ""), commit("Ann", ""));
        assert_eq!(on_insert("", ",", ""), Some(Insert::Swallow));
        assert_eq!(on_insert("", " ", ""), Some(Insert::Swallow));
        assert_eq!(on_insert("bob@example.com", ";", ""), commit("bob@example.com", ""));
    }

    #[test]
    fn a_comma_in_a_quoted_name_is_part_of_it() {
        assert_eq!(on_insert("\"Lee", ",", ""), None);
        assert_eq!(on_insert("\"Lee, Ann\" <ann@exa", ",", ""), None);
    }

    #[test]
    fn a_pasted_list_becomes_pills_and_keeps_its_tail() {
        assert_eq!(
            on_insert("", "ann@example.com, Bob <bob@example.com>, car", ""),
            commit("ann@example.com, Bob <bob@example.com>", "car"),
        );
        assert_eq!(
            on_insert("", "ann@example.com bob@example.com", ""),
            commit("ann@example.com, bob@example.com", ""),
        );
        assert_eq!(on_insert("", "Ann Lee", ""), None);
    }

    #[test]
    fn separators_outside_quotes_read_as_commas() {
        assert_eq!(normalise_separators("a@x.com; b@y.com\nc@z.com"), "a@x.com, b@y.com,c@z.com");
        assert_eq!(normalise_separators("\"Lee; Ann\" <a@x.com>"), "\"Lee; Ann\" <a@x.com>");
    }

    #[test]
    fn whole_addresses() {
        assert!(is_whole_address("ann@example.com"));
        assert!(is_whole_address("\"Lee, Ann\" <ann@example.com>"));
        assert!(!is_whole_address("ann"));
        assert!(!is_whole_address("Ann <ann@example.com"));
        assert!(!is_whole_address("ann@"));
    }
}
