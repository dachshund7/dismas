// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use gtk::{glib, graphene, gsk, prelude::*, subclass::prelude::*};

use crate::{
    cover_picture::{LARGE_SIZE, MEDIUM_SIZE},
    track_cover::TrackCover,
};

// The space between the parts of the player
const SPACING: i32 = 6;

// The style class of the children that grow with the cover, past their
// natural width
const COVER_WIDTH: &str = "cover-width";

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct PlayerBox {}

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerBox {
        const NAME: &'static str = "AmberolPlayerBox";
        type Type = super::PlayerBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PlayerBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    // A vertical box for the player, which gives the cover the room that the
    // rest of the player leaves: the large cover if it fits, the medium one
    // if that fits, and none otherwise. When the rest of the player has
    // its natural height, the room left grows the large cover, e.g. in
    // fullscreen; otherwise the player stays centered around it
    // It's a widget rather than a GtkBox, as a GtkBox is laid out by its
    // layout manager, without calling size_allocate
    impl WidgetImpl for PlayerBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (min, nat) = if orientation == gtk::Orientation::Vertical {
                let children = self.measure_children(orientation, for_size);
                let (min, nat) = children
                    .iter()
                    .fold((0, 0), |(min, nat), (_, m, n)| (min + m, nat + n));
                (min + spacings(&children), nat + spacings(&children))
            } else {
                self.measure_children(orientation, -1)
                    .iter()
                    .fold((0, 0), |(min, nat), (_, m, n)| (min.max(*m), nat.max(*n)))
            };
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let children = self.measure_children(gtk::Orientation::Vertical, width);
            let spacings = spacings(&children);
            let (others_min, others_nat) = children
                .iter()
                .filter(|(widget, _, _)| !widget.is::<TrackCover>())
                .fold((0, 0), |(min, nat), (_, m, n)| (min + m, nat + n));

            // The natural height of the cover is the one with the large
            // cover, and the minimum one is without the cover
            let room = height - spacings - others_min;
            let cover_height = children
                .iter()
                .find_map(|(widget, min, nat)| {
                    widget.downcast_ref::<TrackCover>()?;
                    let medium = nat - LARGE_SIZE + MEDIUM_SIZE;
                    Some(if *nat <= room {
                        *nat
                    } else if medium <= room {
                        medium
                    } else {
                        *min
                    })
                })
                .unwrap_or(0);

            // The rest of the player has its natural height if it fits
            let natural = cover_height + others_nat <= height - spacings;

            // The room left past the natural height of the large cover makes
            // it larger, as far as the width allows, and the waveform under
            // it as wide, so that their edges still line up
            let grow = children
                .iter()
                .find_map(|(widget, _, nat)| {
                    widget.downcast_ref::<TrackCover>()?;
                    if !natural || cover_height != *nat {
                        return Some(0);
                    }
                    let (_, nat_width, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                    let free = height - spacings - others_nat - nat;
                    Some(free.min(width - nat_width).max(0))
                })
                .unwrap_or(0);

            let child_height = |widget: &gtk::Widget, min: i32, nat: i32| {
                if widget.is::<TrackCover>() {
                    cover_height + grow
                } else if natural {
                    nat
                } else {
                    min
                }
            };

            let total = spacings
                + children
                    .iter()
                    .map(|(widget, min, nat)| child_height(widget, *min, *nat))
                    .sum::<i32>();
            let mut y = ((height - total) / 2).max(0);
            for (widget, min, nat) in &children {
                let h = child_height(widget, *min, *nat);
                let (x, w) = if widget.has_css_class(COVER_WIDTH) {
                    let (_, nat_width, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                    let w = (nat_width + grow).min(width);
                    ((width - w) / 2, w)
                } else {
                    (0, width)
                };
                let transform =
                    gsk::Transform::new().translate(&graphene::Point::new(x as f32, y as f32));
                widget.allocate(w, h, -1, Some(transform));
                y += h + SPACING;
            }
        }
    }

    impl PlayerBox {
        // The children to lay out, with their minimum and natural sizes
        fn measure_children(
            &self,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> Vec<(gtk::Widget, i32, i32)> {
            let mut children = Vec::new();
            let mut child = self.obj().first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if widget.should_layout() {
                    let (min, nat, _, _) = widget.measure(orientation, for_size);
                    children.push((widget, min, nat));
                }
            }
            children
        }
    }

    fn spacings(children: &[(gtk::Widget, i32, i32)]) -> i32 {
        SPACING * (children.len() as i32 - 1).max(0)
    }
}

glib::wrapper! {
    pub struct PlayerBox(ObjectSubclass<imp::PlayerBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
