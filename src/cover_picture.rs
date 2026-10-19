// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use glib::clone;
use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

use crate::i18n::i18n;

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "AmberolCoverSize")]
pub enum CoverSize {
    #[default]
    Large = 0,
    Small = 1,
}

impl AsRef<str> for CoverSize {
    fn as_ref(&self) -> &str {
        match self {
            CoverSize::Large => "large",
            CoverSize::Small => "small",
        }
    }
}

// The large cover is as wide as the waveform and the playback controls
// under it, which don't scale with the font either
pub const LARGE_SIZE: i32 = 256;
// The large cover in windows too short for it, the size of the cover
// upstream: three quarters of the controls
pub const MEDIUM_SIZE: i32 = 192;

mod imp {
    use super::*;

    const SMALL_SIZE: i32 = 48;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::CoverPicture)]
    pub struct CoverPicture {
        #[property(get, set = Self::set_cover, explicit_notify, nullable)]
        pub cover: RefCell<Option<gdk::Texture>>,
        #[property(get, set = Self::set_cover_size, explicit_notify, builder(CoverSize::default()))]
        pub cover_size: Cell<CoverSize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CoverPicture {
        const NAME: &'static str = "AmberolCoverPicture";
        type Type = super::CoverPicture;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("picture");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for CoverPicture {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj().add_css_class("cover");
            self.obj().set_overflow(gtk::Overflow::Hidden);

            self.obj().connect_notify_local(
                Some("scale-factor"),
                clone!(
                    #[weak(rename_to = _obj)]
                    self,
                    move |picture, _| {
                        picture.queue_draw();
                    }
                ),
            );

            self.obj()
                .upcast_ref::<gtk::Accessible>()
                .update_property(&[(gtk::accessible::Property::Label(&i18n("Cover image")))]);
        }
    }

    impl WidgetImpl for CoverPicture {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match self.cover_size.get() {
                // The track cover gives the large cover the room it has,
                // up to its size
                CoverSize::Large => (0, LARGE_SIZE, -1, -1),
                CoverSize::Small => (SMALL_SIZE, SMALL_SIZE, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(ref cover) = *self.cover.borrow() {
                let widget = self.obj();
                let scale_factor = f64::from(widget.scale_factor());
                let width = f64::from(widget.width()) * scale_factor;
                let height = f64::from(widget.height()) * scale_factor;
                let ratio = cover.intrinsic_aspect_ratio();

                let (w, h) = if ratio > 1.0 {
                    (width, width / ratio)
                } else {
                    (height * ratio, height)
                };

                let x = (width - w.ceil()) / 2.0;
                let y = (height - h).floor() / 2.0;

                snapshot.save();
                snapshot.scale(1.0 / scale_factor as f32, 1.0 / scale_factor as f32);
                snapshot.translate(&graphene::Point::new(x as f32, y as f32));
                // The small covers are drawn from a copy at their size, which
                // needs no mipmaps; making them for each row is costly
                let filter = match self.cover_size.get() {
                    CoverSize::Large => gsk::ScalingFilter::Trilinear,
                    CoverSize::Small => gsk::ScalingFilter::Linear,
                };
                snapshot.append_scaled_texture(
                    cover,
                    filter,
                    &graphene::Rect::new(0.0, 0.0, w as f32, h as f32),
                );
                snapshot.restore();
            }
        }
    }

    impl CoverPicture {
        fn set_cover(&self, cover: Option<gdk::Texture>) {
            self.cover.replace(cover);

            let obj = self.obj();
            obj.queue_draw();
            obj.notify_cover();
        }

        fn set_cover_size(&self, cover_size: CoverSize) {
            self.cover_size.replace(cover_size);

            let obj = self.obj();
            obj.queue_resize();
            obj.notify_cover_size();
        }
    }
}

glib::wrapper! {
    pub struct CoverPicture(ObjectSubclass<imp::CoverPicture>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for CoverPicture {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl CoverPicture {
    pub fn new() -> Self {
        Self::default()
    }
}
