// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// A container that shows its child only where it fits: it asks for no
// width of its own, and hides the child when it gets less than the child's
// minimum width. The titles of the pages use it, so that the header bars
// can do without them in a narrow window

use std::cell::RefCell;

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::FitBin)]
    pub struct FitBin {
        #[property(get, set = Self::set_child, nullable)]
        pub child: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FitBin {
        const NAME: &'static str = "DismasFitBin";
        type Type = super::FitBin;
        type ParentType = gtk::Widget;
    }

    #[glib::derived_properties]
    impl ObjectImpl for FitBin {
        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for FitBin {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            let (min, nat, min_baseline, nat_baseline) = child.measure(orientation, for_size);
            if orientation == gtk::Orientation::Horizontal {
                (0, nat, -1, -1)
            } else {
                (min, nat, min_baseline, nat_baseline)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            // Hidden children aren't drawn, and don't need an allocation
            let fits = width >= child.measure(gtk::Orientation::Horizontal, -1).0;
            child.set_child_visible(fits);
            if fits {
                child.size_allocate(&gtk::Allocation::new(0, 0, width, height), baseline);
            }
        }
    }

    impl FitBin {
        fn set_child(&self, child: Option<gtk::Widget>) {
            if let Some(old) = self.child.take() {
                old.unparent();
            }
            if let Some(child) = &child {
                child.set_parent(&*self.obj());
            }
            self.child.replace(child);
            self.obj().queue_resize();
        }
    }
}

glib::wrapper! {
    pub struct FitBin(ObjectSubclass<imp::FitBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
