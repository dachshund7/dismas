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

mod imp {
    use super::*;

    const LARGE_SIZE: i32 = 288;
    const SMALL_SIZE: i32 = 48;

    // The reference font resolution the cover sizes were designed for
    const BASE_DPI: f64 = 96.0;

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

            // The large cover scales with the font resolution, so we need
            // to resize when it changes
            self.obj().settings().connect_gtk_xft_dpi_notify(clone!(
                #[weak(rename_to = obj)]
                self.obj(),
                move |_| {
                    obj.queue_resize();
                }
            ));

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
                CoverSize::Large => {
                    let size = (LARGE_SIZE as f64 * self.font_scale()).round() as i32;
                    (size, size, -1, -1)
                }
                CoverSize::Small => (SMALL_SIZE, SMALL_SIZE, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(ref cover) = *self.cover.borrow() {
                let widget = self.obj();
                let scale_factor = widget.scale_factor() as f64;
                let width = widget.width() as f64 * scale_factor;
                let height = widget.height() as f64 * scale_factor;
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
                snapshot.append_scaled_texture(
                    cover,
                    gsk::ScalingFilter::Trilinear,
                    &graphene::Rect::new(0.0, 0.0, w as f32, h as f32),
                );
                snapshot.restore();
            }
        }
    }

    impl CoverPicture {
        // How the font resolution compares to the reference one; this keeps
        // the cover in proportion with the text whether the fonts are scaled
        // through the DPI or through the window scaling factor
        fn font_scale(&self) -> f64 {
            // GtkSettings:gtk-xft-dpi is in 1024ths of a dot per inch, or
            // -1 for the default
            let dpi = self.obj().settings().gtk_xft_dpi();
            if dpi <= 0 {
                return 1.0;
            }

            dpi as f64 / 1024.0 / BASE_DPI
        }

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
