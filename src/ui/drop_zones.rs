//! The drop surfaces a composer shows while files are dragged over it: one to
//! attach them, one to place pictures in the text, one to upload them to
//! cloud storage and share a link. Each appears only when it can take the
//! files being dragged: the text only takes pictures, and only in a rich
//! message; the cloud only when an account is set up.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n, ni18n_f};

/// Which surface the files were let go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropChoice {
    Attach,
    Inline,
    Cloud,
}

const FADE_MS: u32 = 160;

pub struct DropZones {
    layer: gtk::Box,
    zones: gtk::Box,
    summary: gtk::Label,
    attach: Zone,
    inline: Zone,
    cloud: Zone,
    /// Set by the composer as its format changes: pictures go into the text
    /// of a rich message only.
    allow_inline: Cell<bool>,
    /// The cloud accounts' names, for the upload surface's line; empty
    /// means there is nowhere to upload to.
    cloud_names: RefCell<Vec<String>>,
    /// Bumped on every enter and leave, so a file list read for a drag that
    /// has since left does not bring the surfaces up after it.
    epoch: Cell<u32>,
    /// Up, or on its way up; the layer stays visible a moment longer while
    /// it fades out.
    shown: Cell<bool>,
    fade: RefCell<Option<adw::TimedAnimation>>,
}

struct Zone {
    widget: gtk::Box,
    subtitle: gtk::Label,
}

impl DropZones {
    /// Lay the surfaces over `host`'s content (`overlay` is the host's
    /// overlay) and watch `host` for file drags. `on_drop` gets the files
    /// and the surface they landed on.
    pub fn install(
        host: &impl IsA<gtk::Widget>,
        overlay: &gtk::Overlay,
        on_drop: impl Fn(DropChoice, Vec<PathBuf>) + 'static,
    ) -> Rc<Self> {
        let on_drop: Rc<dyn Fn(DropChoice, Vec<PathBuf>)> = Rc::new(on_drop);

        let layer = gtk::Box::new(gtk::Orientation::Vertical, 14);
        layer.add_css_class("drop-layer");
        layer.set_visible(false);

        let summary = gtk::Label::new(None);
        summary.add_css_class("drop-summary");
        summary.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        layer.append(&summary);

        let zones = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        zones.set_homogeneous(true);
        zones.set_vexpand(true);
        layer.append(&zones);

        let attach = Zone::new("mail-attachment-symbolic", &i18n("Attach"));
        let inline = Zone::new("image-x-generic-symbolic", &i18n("Insert in Text"));
        let cloud = Zone::new("cloud-symbolic", &i18n("Upload to Cloud"));
        for z in [&attach, &inline, &cloud] {
            zones.append(&z.widget);
        }

        let this = Rc::new(Self {
            layer: layer.clone(),
            zones,
            summary,
            attach,
            inline,
            cloud,
            allow_inline: Cell::new(true),
            cloud_names: RefCell::new(Vec::new()),
            epoch: Cell::new(0),
            shown: Cell::new(false),
            fade: RefCell::new(None),
        });

        for (zone, choice) in [
            (&this.attach.widget, DropChoice::Attach),
            (&this.inline.widget, DropChoice::Inline),
            (&this.cloud.widget, DropChoice::Cloud),
        ] {
            zone.add_controller(this.target(choice, &on_drop));
        }
        // Let go between the surfaces: attached, which is what a drop on
        // the composer did before there were surfaces to choose from.
        layer.add_controller(this.target(DropChoice::Attach, &on_drop));
        overlay.add_overlay(&layer);

        // A motion controller rather than a drop target: it sees a drag
        // come and go without taking the drop from whatever is under it.
        let motion = gtk::DropControllerMotion::new();
        let weak = Rc::downgrade(&this);
        let host_widget = host.as_ref().clone();
        motion.connect_enter(move |ctrl, _, _| {
            let Some(this) = weak.upgrade() else { return };
            let Some(drop) = ctrl.drop() else { return };
            if !drop.formats().contains_type(gtk::gdk::FileList::static_type()) {
                return;
            }
            let epoch = this.epoch.get().wrapping_add(1);
            this.epoch.set(epoch);
            let weak = Rc::downgrade(&this);
            let host = host_widget.clone();
            drop.read_value_async(
                gtk::gdk::FileList::static_type(),
                gtk::glib::Priority::DEFAULT,
                gtk::gio::Cancellable::NONE,
                move |res| {
                    let Some(this) = weak.upgrade() else { return };
                    if this.epoch.get() != epoch {
                        return;
                    }
                    let Some(paths) = res.ok().and_then(|v| v.get::<gtk::gdk::FileList>().ok()).map(|l| file_paths(&l))
                    else {
                        return;
                    };
                    if !paths.is_empty() {
                        this.show(&paths, host.width(), host.height());
                    }
                },
            );
        });
        let weak = Rc::downgrade(&this);
        motion.connect_leave(move |_| {
            if let Some(this) = weak.upgrade() {
                this.epoch.set(this.epoch.get().wrapping_add(1));
                this.hide();
            }
        });
        host.add_controller(motion);
        this
    }

    pub fn set_allow_inline(&self, on: bool) {
        self.allow_inline.set(on);
    }

    pub fn set_cloud_names(&self, names: Vec<String>) {
        *self.cloud_names.borrow_mut() = names;
    }

    /// HYLKI_SHOWCASE_DROP_ZONES: bring the surfaces up for `paths` as a
    /// drag would, with `hover`'s card marked as under the pointer, since
    /// no drag can be made from a script.
    pub fn showcase(&self, paths: &[PathBuf], host: &gtk::Widget, hover: Option<DropChoice>) {
        self.show(paths, host.width(), host.height());
        // A capture's window is never drawn on screen, so its animations
        // do not advance: the end of the fade is what there is to see.
        if let Some(fade) = self.fade.borrow().as_ref() {
            fade.skip();
        }
        let zone = match hover {
            Some(DropChoice::Attach) => &self.attach.widget,
            Some(DropChoice::Inline) => &self.inline.widget,
            Some(DropChoice::Cloud) => &self.cloud.widget,
            None => return,
        };
        zone.set_state_flags(gtk::StateFlags::DROP_ACTIVE, false);
    }

    fn target(self: &Rc<Self>, choice: DropChoice, on_drop: &Rc<dyn Fn(DropChoice, Vec<PathBuf>)>) -> gtk::DropTarget {
        let target = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        let on_drop = on_drop.clone();
        target.connect_drop(move |_, value, _, _| {
            let Ok(list) = value.get::<gtk::gdk::FileList>() else { return false };
            let paths = file_paths(&list);
            if let Some(this) = weak.upgrade() {
                this.epoch.set(this.epoch.get().wrapping_add(1));
                this.hide();
            }
            if paths.is_empty() {
                return false;
            }
            on_drop(choice, paths);
            true
        });
        target
    }

    fn show(&self, paths: &[PathBuf], width: i32, height: i32) {
        let n = paths.len() as u32;
        let pictures = paths.iter().filter(|p| crate::ui::rich_editor::is_inline_image(p)).count() as u32;
        let size: u64 = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let size = crate::cloud::human_size(size);
        let names = self.cloud_names.borrow();

        self.summary.set_label(&if n == 1 {
            let name = paths[0].file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{name} · {size}")
        } else {
            ni18n_f("{n} file · {size}", "{n} files · {size}", n, &[("n", &n.to_string()), ("size", &size)])
        });

        self.attach.subtitle.set_label(&ni18n("Send it with the message", "Send them with the message", n));

        let inline = self.allow_inline.get() && pictures > 0;
        self.inline.widget.set_visible(inline);
        self.inline.subtitle.set_label(&if pictures == n {
            ni18n("Place the picture in the message", "Place the pictures in the message", n)
        } else {
            i18n("Pictures in the message, other files attached")
        });

        self.cloud.widget.set_visible(!names.is_empty());
        self.cloud.subtitle.set_label(&match names.as_slice() {
            [one] => i18n_f("Share a download link from {name}", &[("name", one)]),
            _ => i18n("Share a download link"),
        });

        // Side by side, unless the composer is too narrow for them and has
        // the height to stack them instead.
        let count = 1 + i32::from(inline) + i32::from(!names.is_empty());
        let side_by_side = width >= height || width >= 190 * count;
        self.zones.set_orientation(if side_by_side {
            gtk::Orientation::Horizontal
        } else {
            gtk::Orientation::Vertical
        });

        if self.shown.replace(true) {
            return;
        }
        if !self.layer.is_visible() {
            self.layer.set_opacity(0.0);
            self.layer.set_visible(true);
        }
        self.fade_to(1.0);
    }

    fn hide(&self) {
        if self.shown.replace(false) {
            self.fade_to(0.0);
        }
    }

    /// Fade the layer from wherever it is: a drag that leaves and comes
    /// back mid-fade turns the fade around rather than starting it over.
    fn fade_to(&self, to: f64) {
        if let Some(running) = self.fade.borrow_mut().take() {
            running.pause();
        }
        let from = self.layer.opacity();
        // Fading out, it is still over the composer for a moment: a second
        // drag arriving then goes to the composer, not to the ghost.
        self.layer.set_can_target(to > 0.0);
        let layer = self.layer.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| layer.set_opacity(v));
        let anim = adw::TimedAnimation::new(&self.layer, from, to, FADE_MS, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        let layer = self.layer.clone();
        anim.connect_done(move |_| {
            if to == 0.0 {
                layer.set_visible(false);
            }
        });
        anim.play();
        *self.fade.borrow_mut() = Some(anim);
    }
}

impl Zone {
    fn new(icon: &str, title: &str) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        widget.add_css_class("drop-zone");
        widget.set_hexpand(true);
        widget.set_vexpand(true);

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_valign(gtk::Align::Center);
        inner.set_vexpand(true);

        let badge = gtk::Image::from_icon_name(icon);
        badge.add_css_class("drop-badge");
        badge.set_pixel_size(30);
        badge.set_halign(gtk::Align::Center);
        badge.set_margin_bottom(6);
        inner.append(&badge);

        let title = gtk::Label::new(Some(title));
        title.add_css_class("drop-title");
        title.set_wrap(true);
        title.set_justify(gtk::Justification::Center);
        inner.append(&title);

        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("drop-subtitle");
        subtitle.set_wrap(true);
        subtitle.set_justify(gtk::Justification::Center);
        subtitle.set_max_width_chars(30);
        // Room for three lines whatever the text, so the icons and titles
        // of cards side by side stay level.
        subtitle.set_lines(3);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.set_valign(gtk::Align::Start);
        inner.append(&subtitle);

        widget.append(&inner);
        Self { widget, subtitle }
    }
}

/// The regular files in a dragged list; folders have nothing to attach.
fn file_paths(list: &gtk::gdk::FileList) -> Vec<PathBuf> {
    list.files().iter().filter_map(|f| f.path()).filter(|p| p.is_file()).collect()
}
