// SPDX-FileCopyrightText: 2022  Maximiliano Sandoval R <msandova@gnome.org>
// SPDX-License-Identifier: GPL-3.0-or-later

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use std::cell::RefCell;

    use adw::subclass::prelude::*;

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
        #[property(get, set = Self::set_drop_target, explicit_notify, nullable)]
        pub drop_target: RefCell<Option<gtk::DropTarget>>,
        pub handler_id: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DragOverlay {
        const NAME: &'static str = "DragOverlay";
        type Type = super::DragOverlay;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("dragoverlay");
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for DragOverlay {
        fn constructed(&self) {
            self.overlay
                .set_parent(self.obj().upcast_ref::<gtk::Widget>());
            self.overlay.add_overlay(&self.revealer);

            self.revealer.set_can_target(false);
            self.revealer
                .set_transition_type(gtk::RevealerTransitionType::Crossfade);
            self.revealer.set_reveal_child(false);

            self.status.set_icon_name(Some("folder-music-symbolic"));
            self.status.add_css_class("drag-overlay-status-page");

            self.revealer.set_child(Some(&self.status));
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

                if let Some(handler_id) = self.handler_id.take() {
                    target.disconnect(handler_id);
                }
            }

            if let Some(ref drop_target) = drop_target {
                let handler_id = drop_target.connect_current_drop_notify(glib::clone!(
                    #[weak(rename_to = revealer)]
                    self.revealer,
                    #[weak(rename_to = overlay)]
                    self.overlay,
                    move |target| {
                        let reveal = target.current_drop().is_some();
                        revealer.set_reveal_child(reveal);
                        if reveal {
                            overlay.child().unwrap().add_css_class("blurred");
                        } else {
                            overlay.child().unwrap().remove_css_class("blurred");
                        }
                    }
                ));
                self.handler_id.replace(Some(handler_id));

                obj.add_controller(drop_target.clone());
            }

            self.drop_target.replace(drop_target);
            obj.notify_drop_target();
        }
    }

    impl WidgetImpl for DragOverlay {}
    impl BinImpl for DragOverlay {}
}

glib::wrapper! {
    pub struct DragOverlay(ObjectSubclass<imp::DragOverlay>)
        @extends gtk::Widget, adw::Bin,
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
}
