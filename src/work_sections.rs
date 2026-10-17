// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// A pass-through list model that groups consecutive movements of the same
// work into sections, so that the queue can separate the works.
// The order of the tracks is not changed: if the movements of a work are not
// next to each other, for instance after shuffling, each run of movements
// gets its own section.

use std::{cell::RefCell, rc::Rc};

use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};

use crate::audio::Track;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct WorkSectionModel {
        pub model: RefCell<Option<gio::ListModel>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WorkSectionModel {
        const NAME: &'static str = "AmberolWorkSectionModel";
        type Type = super::WorkSectionModel;
        type Interfaces = (gio::ListModel, gtk::SectionModel);
    }

    impl ObjectImpl for WorkSectionModel {}

    impl ListModelImpl for WorkSectionModel {
        fn item_type(&self) -> glib::Type {
            Track::static_type()
        }

        fn n_items(&self) -> u32 {
            self.model
                .borrow()
                .as_ref()
                .map_or(0, ListModelExt::n_items)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.model.borrow().as_ref()?.item(position)
        }
    }

    impl SectionModelImpl for WorkSectionModel {
        fn section(&self, position: u32) -> (u32, u32) {
            let model = self.model.borrow();
            let Some(model) = model.as_ref() else {
                return (0, u32::MAX);
            };

            let n_items = model.n_items();
            if position >= n_items {
                return (n_items, u32::MAX);
            }

            let key = section_key(model, position);

            let mut start = position;
            while start > 0 && section_key(model, start - 1) == key {
                start -= 1;
            }

            let mut end = position + 1;
            while end < n_items && section_key(model, end) == key {
                end += 1;
            }

            (start, end)
        }
    }
}

glib::wrapper! {
    pub struct WorkSectionModel(ObjectSubclass<imp::WorkSectionModel>)
        @implements gio::ListModel, gtk::SectionModel;
}

impl WorkSectionModel {
    pub fn new(model: &impl IsA<gio::ListModel>) -> Self {
        let res: Self = glib::Object::new();
        let model = model.upcast_ref::<gio::ListModel>();

        model.connect_items_changed(glib::clone!(
            #[weak]
            res,
            move |_, position, removed, added| {
                res.items_changed(position, removed, added);

                // Changing items can merge or split the sections around them
                let n_items = res.n_items();
                if n_items > 0 {
                    let start = position.saturating_sub(1);
                    let end = (position + added + 1).min(n_items);
                    res.sections_changed(start, end - start);
                }
            }
        ));

        res.imp().model.replace(Some(model.clone()));
        res
    }
}

// The work and composer of a movement; all the tracks that are not
// movements share the same key, so consecutive ones end up in a single
// section
fn section_key(model: &gio::ListModel, position: u32) -> Option<(String, String)> {
    let track = model.item(position)?.downcast::<Track>().ok()?;
    let work = track.movement_work()?;
    Some((work, track.composer()))
}

// The headers of the sections in the queue: a line between each section,
// followed by the name of the work for the movements of a work; a click
// on the name calls `on_work_clicked` with the positions of the first
// movement and past the last one, and whether Ctrl was held
pub fn work_header_factory(
    on_work_clicked: impl Fn(u32, u32, bool) + 'static,
) -> gtk::ListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let on_work_clicked = Rc::new(on_work_clicked);

    factory.connect_setup(move |_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();

        let separator = gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(["work-separator"])
            .build();
        let work_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["work-title"])
            .build();

        // Headers are reused for other sections, so the positions are
        // only read on click
        let click = gtk::GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[weak]
            header,
            #[strong]
            on_work_clicked,
            move |gesture, _, _, _| {
                let ctrl = gesture
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK);
                gesture.set_state(gtk::EventSequenceState::Claimed);
                on_work_clicked(header.start(), header.end(), ctrl);
            }
        ));
        work_label.add_controller(click);

        // A click selects the work only in selection mode, so that's when
        // the title shows it can be clicked
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter(|motion, _, _| {
            let label = motion.widget().unwrap();
            let selecting = label
                .ancestor(gtk::ListView::static_type())
                .is_some_and(|list| list.has_css_class("selecting"));
            label.set_cursor_from_name(selecting.then_some("pointer"));
        });
        work_label.add_controller(motion);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&separator);
        content.append(&work_label);
        header.set_child(Some(&content));
    });

    factory.connect_bind(|_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
        let Some(content) = header.child() else {
            return;
        };
        let separator = content.first_child().unwrap();
        let work_label = content.last_child().and_downcast::<gtk::Label>().unwrap();

        // Nothing to separate at the top of the queue
        separator.set_visible(header.start() > 0);

        let work = header
            .item()
            .and_downcast::<Track>()
            .and_then(|t| t.movement_work());
        work_label.set_visible(work.is_some());
        work_label.set_label(work.as_deref().unwrap_or_default());

        content.set_visible(separator.is_visible() || work_label.is_visible());
    });

    factory.upcast()
}
