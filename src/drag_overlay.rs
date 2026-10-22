// SPDX-FileCopyrightText: 2022  Maximiliano Sandoval R <msandova@gnome.org>
// SPDX-License-Identifier: GPL-3.0-or-later

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

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
        #[property(get, set = Self::set_drop_target, explicit_notify, nullable)]
        pub drop_target: RefCell<Option<gtk::DropTarget>>,
        pub handler_ids: RefCell<Vec<glib::SignalHandlerId>>,
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

            let zones = gtk::Box::new(gtk::Orientation::Vertical, 0);
            zones.add_css_class("drop-zones");
            zones.append(&self.status);
            zones.append(&self.play_status);
            self.revealer.set_child(Some(&zones));
        }

        fn dispose(&self) {
            self.overlay.unparent();
        }
    }
    impl DragOverlay {
        fn set_drop_target(&self, drop_target: Option<gtk::DropTarget>) {
            let obj = self.obj();

            if let Some(target) = self.drop_target.take() {
                obj.remove_controller(&target);

                for handler_id in self.handler_ids.take() {
                    target.disconnect(handler_id);
                }
            }

            if let Some(ref drop_target) = drop_target {
                let handler_id = drop_target.connect_current_drop_notify(glib::clone!(
                    #[weak(rename_to = revealer)]
                    self.revealer,
                    #[weak(rename_to = overlay)]
                    self.overlay,
                    #[weak(rename_to = obj)]
                    obj,
                    move |target| {
                        let reveal = target.current_drop().is_some();
                        revealer.set_reveal_child(reveal);
                        if reveal {
                            overlay.child().unwrap().add_css_class("blurred");
                        } else {
                            overlay.child().unwrap().remove_css_class("blurred");
                            obj.imp().highlight_zone(None);
                        }
                    }
                ));
                // The zone under the pointer is the one the tracks go to
                let highlight = glib::clone!(
                    #[weak(rename_to = obj)]
                    obj,
                    #[upgrade_or]
                    gdk::DragAction::empty(),
                    move |_: &gtk::DropTarget, x: f64, y: f64| {
                        obj.imp().highlight_zone(Some(obj.in_play_zone(x, y)));
                        gdk::DragAction::COPY
                    }
                );
                let enter_id = drop_target.connect_enter(highlight.clone());
                let motion_id = drop_target.connect_motion(highlight);
                self.handler_ids
                    .replace(vec![handler_id, enter_id, motion_id]);

                obj.add_controller(drop_target.clone());
            }

            self.drop_target.replace(drop_target);
            obj.notify_drop_target();
        }

        // With `None`, neither is, when nothing is dragged over them
        fn highlight_zone(&self, play: Option<bool>) {
            for (zone, is_play) in [(&self.status, false), (&self.play_status, true)] {
                if play == Some(is_play) {
                    zone.add_css_class("active");
                } else {
                    zone.remove_css_class("active");
                }
            }
        }
    }

    impl WidgetImpl for DragOverlay {}
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

    // Whether a point, relative to the overlay, is in the zone that plays
    // the tracks; before the zones are first shown, they have no bounds
    // yet, and the tracks are added
    pub fn in_play_zone(&self, x: f64, y: f64) -> bool {
        self.imp()
            .play_status
            .compute_bounds(self)
            .is_some_and(|bounds| {
                bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
            })
    }
}
