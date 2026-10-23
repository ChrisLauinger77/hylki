//! The attachments gallery: listing every attachment across folders.

use super::*;

/// Largest cached attachment whose bytes ride along with a gallery page, so a
/// thumbnail or preview is instant. Anything bigger is fetched when opened.
pub(super) const GALLERY_DATA_CAP: i64 = 6 * 1024 * 1024;

impl AppModel {
    /// Set the attachment scan going over every folder in the gallery's scope,
    /// newest mail first. One chunk per folder per pass; each answer asks for
    /// the next, so the archive fills in behind the user while they browse and
    /// stops as soon as a folder is fully described.
    pub(super) fn start_attachment_scan(&mut self) {
        let folders: Vec<(u32, String)> = self
            .gallery_scope()
            .iter()
            .flat_map(|a| {
                a.folders
                    .iter()
                    .map(|f| (a.id, f.path.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (account_id, folder_path) in folders {
            self.send_to(account_id, MailRequest::ScanAttachments { folder_path });
        }
    }

    /// The accounts and folders the attachments gallery may draw on: every
    /// folder the cache's gallery query can return, which is all of them bar
    /// Drafts, Junk and Trash. The folders are already in sidebar order.
    pub(super) fn gallery_scope(&self) -> Vec<GalleryAccount> {
        self.accounts
            .iter()
            .map(|a| GalleryAccount {
                id: a.id,
                label: a.label.clone(),
                folders: self
                    .folders
                    .get(&a.id)
                    .map(|fs| {
                        fs.iter()
                            .filter(|f| {
                                !matches!(
                                    f.kind,
                                    FolderKind::Drafts
                                        | FolderKind::Templates
                                        | FolderKind::Junk
                                        | FolderKind::Trash
                                )
                            })
                            .map(|f| GalleryFolder {
                                path: f.path.clone(),
                                name: f.name.clone(),
                                kind: f.kind,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Leave the attachments gallery, dropping its data. The gallery loads
    /// every item's bytes eagerly (up to 300 × 6 MiB per account) and reloads
    /// from scratch on each visit anyway, so keeping two copies of that around
    /// for the rest of the session bought nothing but memory (issue #106).
    pub(super) fn leave_gallery(&mut self) {
        if std::mem::take(&mut self.showing_gallery) {
            self.gallery_total = 0;
            self.gallery.emit(GalleryInput::Page {
                items: Vec::new(),
                total: 0,
                offset: 0,
            });
        }
    }
}
