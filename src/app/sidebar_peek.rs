//! The sidebar's icon rail and the peek that slides the full sidebar over
//! the list.

use super::*;

pub(super) const SIDEBAR_RAIL_WIDTH: f64 = 80.0;

impl AppModel {
    /// Pin the floating hover-peek open as the normal side-by-side sidebar:
    /// the arrow inside the peek, clicked at a width that can host the full
    /// sidebar, persists the expanded state: drop the panel, expand the
    /// docked sidebar's rows, and save.
    pub(super) fn pin_sidebar_from_peek(&mut self) {
        tracing::info!("peek: pinned to side-by-side");
        self.sidebar_peek = false;
        if !self.sidebar_layout_borrowed() {
            self.sidebar_collapsed = false;
        }
        self.rail_active = false;
        if let Some(peek) = self.peek_split.clone() {
            self.peek_transition.set(true);
            peek.set_show_sidebar(false);
            self.peek_transition.set(false);
        }
        self.sidebar.emit(SidebarInput::SetCollapsed(false));
        // Side-by-side again: settle at the normal expanded width.
        self.animate_sidebar(false);
        self.compact_sidebar_header(false);
        if !self.sidebar_layout_borrowed() {
            self.save_sidebar_state();
        }
    }

    /// Close the floating sidebar overlay if it is open — navigation picked in
    /// it is done with it (mirrors how GNOME's own adaptive sidebars behave).
    pub(super) fn close_sidebar_peek(&mut self) {
        if self.sidebar_peek {
            self.rail_active = true;
            self.set_sidebar_peek(false, true);
        }
    }

    /// Hide or show the sidebar header's title and window controls (icon rail).
    pub(super) fn compact_sidebar_header(&self, compact: bool) {
        if let (Some(header), Some(title), Some(menu)) = (
            self.sidebar_header.as_ref(),
            self.app_title.as_ref(),
            self.sidebar_menu.as_ref(),
        ) {
            set_sidebar_header_compact(header, title, menu, &self.sidebar_refresh, compact);
        }
    }

    /// Open or close the narrow-window sidebar *peek*: the expanded panel
    /// (the second sidebar instance, with its own header) sliding out over
    /// the panes from the rail's right edge, through the nested split view's
    /// overlay mode. The docked rail is not involved: it keeps its rows,
    /// header and width throughout and stays clickable beside the panel.
    /// `animate` is advisory — libadwaita animates the overlay itself.
    pub(super) fn set_sidebar_peek(&mut self, open: bool, animate: bool) {
        let Some(peek) = self.peek_split.clone() else {
            return;
        };
        tracing::info!(
            "peek: set open={open} animate={animate} (was peek={}, shown={})",
            self.sidebar_peek,
            peek.shows_sidebar()
        );
        self.sidebar_peek = open;
        // The notify our own transition emits must not read as a dismissal.
        self.peek_transition.set(true);
        peek.set_show_sidebar(open);
        self.peek_transition.set(false);
    }

    /// Send a state update to both sidebar instances — the docked rail and
    /// the peek panel show the same accounts, counts and rows.
    pub(super) fn sidebars_emit(&self, msg: SidebarInput) {
        self.peek_sidebar.emit(msg.clone());
        self.sidebar.emit(msg);
    }

    /// Highlight what the app now shows in both sidebar instances. The one
    /// the user clicked already has it (its guard makes this a no-op), the
    /// other follows; neither reports back.
    pub(super) fn mirror_selection(&self, sel: crate::ui::sidebar::Sel) {
        self.sidebars_emit(SidebarInput::MirrorSelection(sel));
    }

    /// Smoothly animate the sidebar rail between its expanded width and the
    /// narrow icon-only width by interpolating the split view's pinned width.
    pub(super) fn animate_sidebar(&mut self, collapsing: bool) {
        let Some(split) = self.sidebar_split.clone() else {
            return;
        };
        // Start from the current on-screen width; fall back to sensible defaults.
        let from = split
            .sidebar()
            .map(|w| w.width() as f64)
            .filter(|w| *w > 1.0)
            .unwrap_or(if collapsing {
                256.0
            } else {
                SIDEBAR_RAIL_WIDTH
            });
        let expanded = (0.2 * self.window.width() as f64).clamp(220.0, 280.0);
        let to = if collapsing {
            SIDEBAR_RAIL_WIDTH
        } else {
            expanded
        };

        let s = split.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| {
            s.set_min_sidebar_width(v);
            s.set_max_sidebar_width(v);
        });
        let anim = adw::TimedAnimation::new(&split, from, to, 200, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        if !collapsing {
            // Restore responsive sizing once expanded, so window resizes track again.
            let s2 = split.clone();
            anim.connect_done(move |_| {
                s2.set_min_sidebar_width(180.0);
                s2.set_max_sidebar_width(280.0);
                s2.set_sidebar_width_fraction(0.2);
            });
        }
        anim.play();
        self.sidebar_anim = Some(anim);
    }

    /// Whether the icon rail marks unread folders with a dot instead of a
    /// count: the preference, or Focus Mode's rail part while it holds.
    pub(super) fn rail_dots_now(&self) -> bool {
        self.rail_dots || self.focus.active(config::FocusPart::RailSidebar)
    }

    /// Whether the rail belongs on screen: the window is too narrow for the
    /// full sidebar, Focus Mode has folded it, or the user collapsed it
    /// themselves. Only the last of those is ever saved.
    pub(super) fn rail_wanted(&self) -> bool {
        self.auto_rail
            || self.focus.active(config::FocusPart::RailSidebar)
            || self.sidebar_collapsed
    }

    /// Whether the sidebar's layout is Focus Mode's for the while: the rail
    /// part is in force, so collapsing or expanding it is a look for as long
    /// as the mode lasts and never the layout the user keeps. Nothing is
    /// saved while this holds, and ending the mode gives back exactly the
    /// sidebar it took over — whatever was done to it meanwhile.
    pub(super) fn sidebar_layout_borrowed(&self) -> bool {
        self.focus.active(config::FocusPart::RailSidebar)
    }

    /// Put the rail up or take it down to match [`Self::rail_wanted`],
    /// animating the split the way the toggle does. Persists nothing.
    pub(super) fn sync_rail(&mut self) {
        let want = self.rail_wanted();
        // A floating peek is the expanded sidebar by another name: fold it
        // before the rail goes up under it.
        if want && self.sidebar_peek {
            self.set_sidebar_peek(false, true);
        }
        if want != self.rail_active {
            self.rail_active = want;
            self.sidebar.emit(SidebarInput::SetCollapsed(want));
            self.animate_sidebar(want);
            self.compact_sidebar_header(want);
        }
    }
}
