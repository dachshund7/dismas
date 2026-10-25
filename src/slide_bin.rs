// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::{prelude::*, subclass::prelude::*};
use gtk::{glib, graphene};

// How long a row takes to slide into its new place, and how it speeds up
// and slows down: an ease-out would start at full speed, which looks like a
// jump for a row dropped far from its place
const SLIDE_DURATION_MS: u32 = 300;
const SLIDE_EASING: adw::Easing = adw::Easing::EaseInOutCubic;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct SlideBin {
        pub child: RefCell<Option<gtk::Widget>>,
        // How far above (negative) or below its place the child is drawn
        pub offset: Cell<f64>,
        pub animation: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SlideBin {
        const NAME: &'static str = "DismasSlideBin";
        type Type = super::SlideBin;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for SlideBin {
        fn dispose(&self) {
            if let Some(animation) = self.animation.take() {
                animation.skip();
            }
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SlideBin {
        // The child is moved as a whole, its CSS background included,
        // which it can't do by drawing itself somewhere else
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let offset = self.offset.get();
            if offset == 0.0 {
                self.parent_snapshot(snapshot);
                return;
            }
            snapshot.save();
            snapshot.translate(&graphene::Point::new(0.0, offset as f32));
            self.parent_snapshot(snapshot);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    // Draws its child away from its place, to slide it there after it
    // moved, as rows do when tracks are moved in the queue
    pub struct SlideBin(ObjectSubclass<imp::SlideBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SlideBin {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let bin: Self = glib::Object::new();
        child.set_parent(&bin);
        bin.imp().child.replace(Some(child.clone().upcast()));
        bin
    }

    pub fn child(&self) -> Option<gtk::Widget> {
        self.imp().child.borrow().clone()
    }

    // Where the child is drawn, in the coordinates of another widget,
    // while it slides
    pub fn shown_y(&self, target: &impl IsA<gtk::Widget>) -> Option<f64> {
        let point = self.compute_point(target, &graphene::Point::zero())?;
        Some(f64::from(point.y()) + self.imp().offset.get())
    }

    // Slides the child to its place from where it was drawn before, in
    // the coordinates of another widget
    pub fn slide_from(&self, y: f64, target: &impl IsA<gtk::Widget>) {
        self.stop_sliding();
        let Some(point) = self.compute_point(target, &graphene::Point::zero()) else {
            return;
        };
        let offset = y - f64::from(point.y());
        if offset.abs() < 1.0 {
            return;
        }

        let animation_target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |value| {
                this.imp().offset.set(value);
                this.queue_draw();
            }
        ));
        let animation = adw::TimedAnimation::builder()
            .widget(self)
            .value_from(offset)
            .value_to(0.0)
            .duration(SLIDE_DURATION_MS)
            .easing(SLIDE_EASING)
            .target(&animation_target)
            .build();
        self.imp().animation.replace(Some(animation.clone()));
        // The animation starts at the next frame, but the child must
        // already be drawn in its old place in this one
        self.imp().offset.set(offset);
        self.queue_draw();
        animation.play();
    }

    // Puts the child back in its place, as when it shows another item
    pub fn stop_sliding(&self) {
        if let Some(animation) = self.imp().animation.take() {
            animation.skip();
        }
        self.imp().offset.set(0.0);
        self.queue_draw();
    }
}
