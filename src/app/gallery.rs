//! The attachments gallery: listing every attachment across folders.

/// Largest cached attachment whose bytes ride along with a gallery page, so a
/// thumbnail or preview is instant. Anything bigger is fetched when opened.
pub(super) const GALLERY_DATA_CAP: i64 = 6 * 1024 * 1024;
