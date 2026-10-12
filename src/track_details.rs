// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::subclass::prelude::*;
use gtk::{glib, prelude::*, CompositeTemplate};

use crate::marquee::Marquee;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/track-details.ui")]
    pub struct TrackDetails {
        // Template widgets
        #[template_child]
        pub track_title_label: TemplateChild<Marquee>,
        #[template_child]
        pub track_movement_label: TemplateChild<Marquee>,
        #[template_child]
        pub track_artist_label: TemplateChild<Marquee>,
        #[template_child]
        pub track_album_label: TemplateChild<Marquee>,
        #[template_child]
        pub track_credits_label: TemplateChild<Marquee>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TrackDetails {
        const NAME: &'static str = "AmberolTrackDetails";
        type Type = super::TrackDetails;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("trackdetails");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TrackDetails {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TrackDetails {}
}

glib::wrapper! {
    pub struct TrackDetails(ObjectSubclass<imp::TrackDetails>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TrackDetails {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TrackDetails {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn artist_label(&self) -> Marquee {
        self.imp().track_artist_label.get()
    }

    pub fn title_label(&self) -> Marquee {
        self.imp().track_title_label.get()
    }

    pub fn album_label(&self) -> Marquee {
        self.imp().track_album_label.get()
    }

    pub fn movement_label(&self) -> Marquee {
        self.imp().track_movement_label.get()
    }

    pub fn credits_label(&self) -> Marquee {
        self.imp().track_credits_label.get()
    }
}
