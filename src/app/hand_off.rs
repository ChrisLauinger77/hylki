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
