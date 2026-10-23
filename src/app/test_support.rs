//! Fixtures shared by the app's test modules.

use crate::models::Message;

pub(super) fn msg(account_id: u32, folder_id: u32, uid: u32) -> Message {
    Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: String::new(),
        from_addr: String::new(),
        reply_to: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject: String::new(),
        preview: String::new(),
        body: String::new(),
        date: String::new(),
        timestamp: 0,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: false,
        message_id: String::new(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    }
}

/// A conversation member as the related-lookup hands it over: what matters
/// here is which folder it came from and what Message-ID it carries.
pub(super) fn labelled(folder_id: u32, uid: u32, message_id: &str) -> Message {
    let mut m = msg(1, folder_id, uid);
    m.message_id = message_id.to_string();
    m
}
