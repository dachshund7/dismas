// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// A container that shows its child as a square as wide as it's given,
// between a minimum and a maximum size, and centered in the rest; the
// covers of the album grid use it to grow with the width of the columns

use std::cell::Cell;

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct SquareBin {
        pub min_size: Cell<i32>,
        pub max_size: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SquareBin {
        const NAME: &'static str = "DismasSquareBin";
        type Type = super::SquareBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SquareBin {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SquareBin {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let min = self.min_size.get();
            let max = self.max_size.get();
            if orientation == gtk::Orientation::Horizontal {
                return (min, max, -1, -1);
            }
            let size = if for_size < 0 {
                min
            } else {
                for_size.clamp(min, max)
            };
            (size, size, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            let size = width.min(height).min(self.max_size.get());
            // GTK expects children to be measured before they're allocated
            child.measure(gtk::Orientation::Horizontal, -1);
            child.size_allocate(
                &gtk::Allocation::new((width - size) / 2, (height - size) / 2, size, size),
                baseline,
            );
        }
    }
}

glib::wrapper! {
    pub struct SquareBin(ObjectSubclass<imp::SquareBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SquareBin {
    pub fn new(child: &impl IsA<gtk::Widget>, min_size: i32, max_size: i32) -> Self {
        let bin: Self = glib::Object::new();
        bin.imp().min_size.set(min_size);
        bin.imp().max_size.set(max_size);
        child.set_parent(&bin);
        bin
    }
}
