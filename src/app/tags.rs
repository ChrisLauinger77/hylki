//! Tags: their colors, the tag views, and the tag finder.

use super::*;

/// A running tag-finder scan (see `AppMsg::FindTags`).
pub(super) struct TagScan {
    pub(super) gen: u32,
    pub(super) remaining: usize,
    pub(super) found: Vec<(u32, Vec<KeywordFinding>)>,
}
