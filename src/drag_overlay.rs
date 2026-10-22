// SPDX-FileCopyrightText: 2022  Maximiliano Sandoval R <msandova@gnome.org>
// SPDX-License-Identifier: GPL-3.0-or-later

// The zones shown over the window while files are dragged over it: the
// larger one adds the files, and the one below it plays them. The window
// has the drop target, and tells the overlay where to show the zones, as
// files dragged over the playlist pane go in between its tracks instead

use gtk::{glib, graphene, prelude::*, subclass::prelude::*};

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::DragOverlay)]
    pub struct DragOverlay {
        #[property(
            name = "child",
            get = |imp: &Self| imp.overlay.child(),
            set = |imp: &Self, child: Option<gtk::Widget>| imp.overlay.set_child(child.as_ref()),
            nullable,
            type = Option<gtk::Widget>,
        )]
        pub overlay: gtk::Overlay,
        pub revealer: gtk::Revealer,
        pub zones: gtk::Box,
        #[property(
            name = "title",
            get = |imp: &Self| imp.status.title(),
            set = |imp: &Self, title: glib::GString| imp.status.set_title(&title),
            type = glib::GString,
        )]
        pub status: adw::StatusPage,
        // Below the one to add tracks, which is the larger, as dropping
        // there does no harm
        #[property(
            name = "play-title",
            get = |imp: &Self| imp.play_status.title(),
            set = |imp: &Self, title: glib::GString| imp.play_status.set_title(&title),
            type = glib::GString,
        )]
        pub play_status: adw::StatusPage,
        // The widget blurred under the zones
        pub blurred: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DragOverlay {
        const NAME: &'static str = "DragOverlay";
        type Type = super::DragOverlay;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("dragoverlay");
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for DragOverlay {
        fn constructed(&self) {
            self.parent_constructed();

            self.overlay
                .set_parent(self.obj().upcast_ref::<gtk::Widget>());
            self.overlay.add_overlay(&self.revealer);

            self.revealer.set_can_target(false);
            self.revealer
                .set_transition_type(gtk::RevealerTransitionType::Crossfade);
            self.revealer.set_reveal_child(false);

            self.status.set_icon_name(Some("folder-music-symbolic"));
            self.status.add_css_class("drop-zone");
            self.status.set_vexpand(true);

            self.play_status
                .set_icon_name(Some("media-playback-start-symbolic"));
            self.play_status.add_css_class("drop-zone");
            self.play_status.add_css_class("compact");

            self.zones.set_orientation(gtk::Orientation::Vertical);
            self.zones.add_css_class("drop-zones");
            self.zones.append(&self.status);
            self.zones.append(&self.play_status);
            self.revealer.set_child(Some(&self.zones));
        }

        fn dispose(&self) {
            self.overlay.unparent();
        }
    }

    impl WidgetImpl for DragOverlay {}

    impl DragOverlay {
        pub(super) fn set_blurred(&self, widget: Option<&gtk::Widget>) {
            if self.blurred.borrow().as_ref() == widget {
                return;
            }
            if let Some(old) = self.blurred.replace(widget.cloned()) {
                old.remove_css_class("blurred");
            }
            if let Some(widget) = widget {
                widget.add_css_class("blurred");
            }
        }

        // With `None`, neither is, when nothing is dragged over them
        pub(super) fn highlight_zone(&self, play: Option<bool>) {
            for (zone, is_play) in [(&self.status, false), (&self.play_status, true)] {
                if play == Some(is_play) {
                    zone.add_css_class("active");
                } else {
                    zone.remove_css_class("active");
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct DragOverlay(ObjectSubclass<imp::DragOverlay>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for DragOverlay {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl DragOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    // Shows the zones across the overlay from `left` to `right`, over the
    // widget that is blurred, and highlights the one under a point; both
    // are relative to the overlay
    pub fn show_zones(&self, left: f32, right: f32, blurred: &gtk::Widget, x: f64, y: f64) {
        let imp = self.imp();
        let margin_start = left.round() as i32;
        let margin_end = (self.width() - right.round() as i32).max(0);
        if imp.zones.margin_start() != margin_start {
            imp.zones.set_margin_start(margin_start);
        }
        if imp.zones.margin_end() != margin_end {
            imp.zones.set_margin_end(margin_end);
        }
        imp.set_blurred(Some(blurred));
        imp.revealer.set_reveal_child(true);
        imp.highlight_zone(Some(self.in_play_zone(x, y)));
    }

    pub fn hide_zones(&self) {
        let imp = self.imp();
        imp.revealer.set_reveal_child(false);
        imp.set_blurred(None);
        imp.highlight_zone(None);
    }

    // Whether a point, relative to the overlay, is in the zone that plays
    // the tracks; before the zones are first shown, they have no bounds
    // yet, and the tracks are added
    pub fn in_play_zone(&self, x: f64, y: f64) -> bool {
        self.imp()
            .play_status
            .compute_bounds(self)
            .is_some_and(|bounds| bounds.contains_point(&graphene::Point::new(x as f32, y as f32)))
    }
}
