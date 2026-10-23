//! The tray icon and its menu, and the launcher badge.

/// What identifies a tray mail list for change detection: the total, then
/// each card.
pub(super) fn tray_mail_key(mail: &crate::tray::TrayMailList) -> Vec<(u32, u32, String, String)> {
    std::iter::once((u32::MAX, mail.unread, String::new(), String::new()))
        .chain(mail.items.iter().map(|m| {
            (
                m.account_id,
                m.message_id,
                m.heading.clone(),
                m.subject.clone(),
            )
        }))
        .collect()
}
