// SPDX-FileCopyrightText: 2023  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib, graphene, gsk, prelude::*};

use crate::cover_picture::{CoverPicture, LARGE_SIZE, MEDIUM_SIZE};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/track-cover.ui")]
    pub struct TrackCover {
        // Template widgets
        #[template_child]
        pub cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub album_image: TemplateChild<CoverPicture>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TrackCover {
        const NAME: &'static str = "DismasTrackCover";
        type Type = super::TrackCover;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

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

    // The cover takes the room that the rest of the player leaves, so that
    // the player fits in short windows: it's large when it fits, as large
    // as the room it gets, medium when that fits, and hidden otherwise. It is always a square, as the
    // shadow and the rounded corners of the card are drawn around it
    impl WidgetImpl for TrackCover {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let natural = self
                .obj()
                .first_child()
                .map_or(0, |child| child.measure(orientation, -1).1);
            (0, natural, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };

            // The room around the image, for its margins
            let (min_width, nat_width, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            let (min_height, nat_height, _, _) = child.measure(gtk::Orientation::Vertical, -1);
            let extra_width = (nat_width - LARGE_SIZE).max(0);
            let extra_height = (nat_height - LARGE_SIZE).max(0);

            let room = (width - extra_width).min(height - extra_height);
            let size = if room >= LARGE_SIZE {
                Some(room)
            } else if room >= MEDIUM_SIZE {
                Some(MEDIUM_SIZE)
            } else {
                None
            };

            // Hiding the child doesn't queue a resize, unlike hiding the
            // widget, which can't be done while allocating it
            child.set_child_visible(size.is_some());
            let (child_width, child_height) = match size {
                Some(size) => (size + extra_width, size + extra_height),
                None => (min_width, min_height),
            };

            let x = (width - child_width) / 2;
            let y = (height - child_height) / 2;
            let transform =
                gsk::Transform::new().translate(&graphene::Point::new(x as f32, y as f32));
            child.allocate(child_width, child_height, -1, Some(transform));
        }
    }
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
