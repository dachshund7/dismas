// SPDX-FileCopyrightText: 2023  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib, prelude::*};

use crate::cover_picture::CoverPicture;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/track-cover.ui")]
    pub struct TrackCover {
        // Template widgets
        #[template_child]
        pub cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub album_image: TemplateChild<CoverPicture>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TrackCover {
        const NAME: &'static str = "AmberolTrackCover";
        type Type = super::TrackCover;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("trackcover");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            CoverPicture::static_type();
            obj.init_template();
        }
    }

    impl ObjectImpl for TrackCover {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TrackCover {}
}

glib::wrapper! {
    pub struct TrackCover(ObjectSubclass<imp::TrackCover>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TrackCover {
    fn default() -> Self {
        glib::Object::new::<Self>()
    }
}

impl TrackCover {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn album_image(&self) -> CoverPicture {
        self.imp().album_image.get()
    }

    pub fn show_cover_image(&self, has_image: bool) {
        let cover_stack = self.imp().cover_stack.get();
        if has_image {
            cover_stack.set_visible_child_name("cover-image");
        } else {
            cover_stack.set_visible_child_name("no-image");
        }
    }
}
