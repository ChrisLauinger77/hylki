//! Files handed to the app (Files' Send with Hylki, a mailto with attach=, a
//! drop on the window): which composer they go to, and how.

use super::*;

/// The handed-in files' names, joined, for a dialog.
pub(super) fn hand_off_names(files: &[std::path::PathBuf]) -> String {
    files
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the handed-in files add up to on disk.
pub(super) fn hand_off_size(files: &[std::path::PathBuf]) -> u64 {
    files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// The "remember this" check a hand-off dialog carries: on, the answer
/// becomes the Settings → System → GNOME Files preference.
pub(super) fn remember_check() -> gtk::CheckButton {
    let check = gtk::CheckButton::with_label(&i18n(
        "Always do this (change it in Settings → System → GNOME Files)",
    ));
    check.set_halign(gtk::Align::Start);
    check.set_margin_top(6);
    check
}

/// Files dropped on the main window, for [`AppMsg::DropFiles`]. Folders
/// are passed over: there is nothing to attach.
pub(super) fn window_drop_target(
    sender: &ComponentSender<AppModel>,
    phase: gtk::PropagationPhase,
) -> gtk::DropTarget {
    let drop = gtk::DropTarget::new(
        gtk::gdk::FileList::static_type(),
        gtk::gdk::DragAction::COPY,
    );
    drop.set_propagation_phase(phase);
    let s = sender.input_sender().clone();
    drop.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gtk::gdk::FileList>() else {
            return false;
        };
        let paths: Vec<_> = list
            .files()
            .iter()
            .filter_map(|f| f.path())
            .filter(|p| p.is_file())
            .collect();
        if paths.is_empty() {
            return false;
        }
        s.emit(AppMsg::DropFiles(paths));
        true
    });
    drop
}

/// Files handed in from outside the app (GNOME Files' "Send with Hylki",
/// "Open With Hylki", a mailto: with attach=), on their way to a composer.
#[derive(Debug, Default)]
pub(crate) struct FileHandOff {
    /// The composer's other fields (a mailto's recipient, subject, body):
    /// used when the files go into a new message.
    pub(super) base: ComposePrefill,
    /// The readable files.
    pub(super) files: Vec<std::path::PathBuf>,
    /// The ones that were named but could not be read (reported once).
    pub(super) dropped: Vec<std::path::PathBuf>,
}

/// Where a hand-off's files are going, once decided.
#[derive(Debug)]
pub(crate) enum HandOffTarget {
    New,
    Draft(Message),
    Reply(Message),
}

/// A hand-off's files split by how the composer takes them: attached, or
/// uploaded to cloud storage and linked (the ones over the size limit).
#[derive(Debug, Default, Clone)]
pub(crate) struct HandOffFiles {
    pub(super) attach: Vec<std::path::PathBuf>,
    pub(super) cloud: Vec<std::path::PathBuf>,
}

impl HandOffFiles {
    pub(super) fn of(files: Vec<std::path::PathBuf>, cloud: bool) -> Self {
        if cloud {
            Self {
                attach: Vec::new(),
                cloud: files,
            }
        } else {
            Self {
                attach: files,
                cloud: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_off_files_go_to_one_side() {
        let files = vec![std::path::PathBuf::from("/a"), "/b".into()];
        let attached = HandOffFiles::of(files.clone(), false);
        assert_eq!((attached.attach.len(), attached.cloud.len()), (2, 0));
        let linked = HandOffFiles::of(files, true);
        assert_eq!((linked.attach.len(), linked.cloud.len()), (0, 2));
    }
}
