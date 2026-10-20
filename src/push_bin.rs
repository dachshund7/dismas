// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The content of the split view, which a collapsed split view covers with
// its sidebar; drawing the content next to the sidebar instead pushes it
// aside, as the sidebar does when the split view isn't collapsed

use std::cell::{Cell, RefCell};

use adw::{prelude::*, subclass::prelude::*};
use gtk::{glib, graphene};

// How many frames the sidebar has to stay in place before the content
// stops following it
const SETTLED_FRAMES: u32 = 10;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct PushBin {
        pub split_view: glib::WeakRef<adw::OverlaySplitView>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub tick_id: RefCell<Option<gtk::TickCallbackId>>,
        // How far the child was last drawn from its place
        pub offset: Cell<f32>,
        pub settled_frames: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PushBin {
        const NAME: &'static str = "DismasPushBin";
        type Type = super::PushBin;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for PushBin {
        fn constructed(&self) {
            self.parent_constructed();

            // The pushed part of the content goes out of the window
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            self.obj().stop_following();
        }
    }

    impl WidgetImpl for PushBin {
        // The split view moves the sidebar as it shows and hides it, but
        // doesn't tell where it is
        fn root(&self) {
            self.parent_root();

            let obj = self.obj();
            let Some(split_view) = obj
                .ancestor(adw::OverlaySplitView::static_type())
                .and_downcast::<adw::OverlaySplitView>()
            else {
                return;
            };

            let handlers = [
                "collapsed",
                "show-sidebar",
                "max-sidebar-width",
                "sidebar-width-fraction",
            ]
            .into_iter()
            .map(|name| {
                split_view.connect_notify_local(
                    Some(name),
                    glib::clone!(
                        #[weak]
                        obj,
                        move |_, _| obj.follow_sidebar()
                    ),
                )
            })
            .collect();
            self.handlers.replace(handlers);
            self.split_view.set(Some(&split_view));
        }

        fn unroot(&self) {
            self.obj().stop_following();
            self.parent_unroot();
        }

        // The child is moved as a whole, its CSS background included; only
        // its drawing moves, since a click outside of the sidebar hides it
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let offset = self.obj().sidebar_offset();
            if offset != self.offset.replace(offset) {
                self.settled_frames.set(0);
            }
            if offset == 0.0 {
                self.parent_snapshot(snapshot);
                return;
            }
            snapshot.save();
            snapshot.translate(&graphene::Point::new(offset, 0.0));
            self.parent_snapshot(snapshot);
            snapshot.restore();
        }
    }

    impl BinImpl for PushBin {}
}

glib::wrapper! {
    pub struct PushBin(ObjectSubclass<imp::PushBin>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PushBin {
    // How far the sidebar covers the content from its edge: positive
    // from the left, negative from the right
    fn sidebar_offset(&self) -> f32 {
        let Some(split_view) = self.imp().split_view.upgrade() else {
            return 0.0;
        };
        if !split_view.is_collapsed() {
            return 0.0;
        }
        let Some(sidebar) = split_view.sidebar().filter(WidgetExt::is_mapped) else {
            return 0.0;
        };
        let Some(bounds) = sidebar.compute_bounds(self) else {
            return 0.0;
        };

        let width = self.width() as f32;
        let (left, right) = (bounds.x(), bounds.x() + bounds.width());
        if left + right < width {
            right.clamp(0.0, width)
        } else {
            (left - width).clamp(-width, 0.0)
        }
    }

    // Draws the content again at each frame, after the sidebar moved in
    // it, until the sidebar stays in place
    fn follow_sidebar(&self) {
        let imp = self.imp();
        imp.settled_frames.set(0);
        self.queue_draw();
        if imp.tick_id.borrow().is_some() {
            return;
        }

        let id = self.add_tick_callback(|bin, _| {
            let imp = bin.imp();
            let frames = imp.settled_frames.get() + 1;
            imp.settled_frames.set(frames);
            if frames > SETTLED_FRAMES {
                imp.tick_id.take();
                bin.repick_pointer();
                return glib::ControlFlow::Break;
            }
            bin.queue_draw();
            glib::ControlFlow::Continue
        });
        imp.tick_id.replace(Some(id));
    }

    // GTK sends clicks to the widget the pointer last moved over, so with
    // the pointer kept still, the next click would go to the widget that
    // the sidebar covers, such as the button that showed it. Content that
    // becomes insensitive makes GTK look again for what's under the pointer;
    // it's made sensitive again before it's drawn, so it doesn't show
    fn repick_pointer(&self) {
        let Some(split_view) = self.imp().split_view.upgrade() else {
            return;
        };
        if split_view.is_collapsed() && split_view.shows_sidebar() && self.is_sensitive() {
            self.set_sensitive(false);
            self.set_sensitive(true);
        }
    }

    fn stop_following(&self) {
        let imp = self.imp();
        if let Some(id) = imp.tick_id.take() {
            id.remove();
        }
        if let Some(split_view) = imp.split_view.upgrade() {
            for id in imp.handlers.take() {
                split_view.disconnect(id);
            }
        }
        imp.split_view.set(None);
    }
}
