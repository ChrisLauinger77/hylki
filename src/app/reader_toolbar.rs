//! The reader header's action buttons: their order, folding into the
//! overflow menu when the window is narrow, and the menu itself.

use super::*;

/// Fallback threshold for folding the reader header's right-hand actions
/// into the overflow menu. Normally the threshold is *measured* at startup from the
/// real row (see the breakpoint in init) so it tracks the user's decoration
/// layout; this value only stands in if that measurement comes back empty.
pub(super) const READER_ACTIONS_BREAKPOINT: f64 = 490.0;

/// The reader header's action buttons and fold breakpoint, kept for
/// re-packing when the layout changes (see relayout_reader_toolbar).
pub(super) struct ReaderToolbarWidgets {
    /// Each action button inside its Focus Mode revealer.
    pub(super) buttons: Vec<(config::ToolbarItem, gtk::Widget)>,
    /// The Outbox's own buttons' revealers (Edit, Send, Send all).
    pub(super) outbox: Vec<gtk::Revealer>,
    pub(super) spinner: gtk::Widget,
    pub(super) bin: adw::BreakpointBin,
    pub(super) breakpoint: adw::Breakpoint,
    /// The row's cost with every action button gone (padding, spacing…).
    pub(super) base: i32,
    /// One action button's natural width.
    pub(super) button_w: i32,
    /// The window controls' width (re-measured when the decoration layout
    /// changes).
    pub(super) controls: std::cell::Cell<i32>,
}
