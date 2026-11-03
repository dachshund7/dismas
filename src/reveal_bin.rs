// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// A child that slides open and closed under what's above it, as a work of
// the list of works opens to show the covers of its albums: the bin is as
// tall as the share of its child that shows, and cuts it off at the bottom

use std::cell::{Cell, RefCell};

use adw::{prelude::*, subclass::prelude::*};
use gtk::glib;

use crate::slide_bin::{SLIDE_DURATION_MS, SLIDE_EASING};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct RevealBin {
        pub(crate) child: RefCell<Option<gtk::Widget>>,
        // The share of the child that shows, from 0 when it's closed to 1
        pub(crate) progress: Cell<f64>,
        pub(crate) animation: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RevealBin {
        const NAME: &'static str = "DismasRevealBin";
        type Type = super::RevealBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for RevealBin {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(animation) = self.animation.take() {
                animation.skip();
            }
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RevealBin {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        // Closed, it takes no room, and its child isn't measured
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let progress = self.progress.get();
            let Some(child) = self.child.borrow().clone().filter(|_| progress > 0.0) else {
                return (0, 0, -1, -1);
            };
            if orientation == gtk::Orientation::Horizontal {
                let (min, natural, _, _) = child.measure(orientation, -1);
                return (min, natural, -1, -1);
            }
            let (_, natural, _, _) = child.measure(orientation, for_size);
            let shown = (f64::from(natural) * progress).round() as i32;
            (shown, shown, -1, -1)
        }

        // The child is as tall as it would be open, and the part below the
        // bin is cut off
        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            if !child.is_child_visible() {
                return;
            }
            let (_, natural, _, _) = child.measure(gtk::Orientation::Vertical, width);
            child.allocate(width, natural.max(height), -1, None);
        }

        // A list's item gives the focus to the first of its children that
        // takes it, and the arrows go from item to item, so they would go
        // to the first child of an open bin rather than to what's above it
        fn grab_focus(&self) -> bool {
            false
        }
    }
}

glib::wrapper! {
    pub(crate) struct RevealBin(ObjectSubclass<imp::RevealBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl RevealBin {
    pub(crate) fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let bin: Self = glib::Object::new();
        child.set_parent(&bin);
        child.set_child_visible(false);
        bin.imp().child.replace(Some(child.clone().upcast()));
        bin
    }

    pub(crate) fn child(&self) -> Option<gtk::Widget> {
        self.imp().child.borrow().clone()
    }

    // The share of the child that shows, 1 once it's open
    pub(crate) fn progress(&self) -> f64 {
        self.imp().progress.get()
    }

    // Opens or closes the bin, sliding it as its child moves to its new
    // place, or at once
    pub(crate) fn set_revealed(&self, revealed: bool, animate: bool) {
        let imp = self.imp();
        if let Some(animation) = imp.animation.take() {
            animation.pause();
        }
        let target = if revealed { 1.0 } else { 0.0 };
        if !animate || imp.progress.get() == target {
            self.set_progress(target);
            return;
        }
        let animation = adw::TimedAnimation::new(
            self,
            imp.progress.get(),
            target,
            SLIDE_DURATION_MS,
            adw::CallbackAnimationTarget::new(glib::clone!(
                #[weak(rename_to = bin)]
                self,
                move |progress| bin.set_progress(progress)
            )),
        );
        animation.set_easing(SLIDE_EASING);
        animation.play();
        imp.animation.replace(Some(animation));
    }

    fn set_progress(&self, progress: f64) {
        let imp = self.imp();
        if imp.progress.replace(progress) == progress {
            return;
        }
        if let Some(child) = imp.child.borrow().as_ref() {
            // Closed, its child can't be focused, and isn't read out
            child.set_child_visible(progress > 0.0);
        }
        self.queue_resize();
    }
}
