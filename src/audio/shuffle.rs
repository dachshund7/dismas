// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;

use glib::clone;
use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use rand::prelude::*;

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::ShuffleListModel)]
    pub struct ShuffleListModel {
        #[property(get, set = Self::set_model, explicit_notify, nullable)]
        pub model: RefCell<Option<gio::ListModel>>,
        pub shuffle: RefCell<Option<Vec<u32>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ShuffleListModel {
        const NAME: &'static str = "ShuffleListModel";
        type Type = super::ShuffleListModel;
        type Interfaces = (gio::ListModel,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for ShuffleListModel {}

    impl ShuffleListModel {
        fn set_model(&self, model: Option<gio::ListModel>) {
            if let Some(ref model) = model {
                model.connect_items_changed(clone!(
                    #[strong(rename_to = this)]
                    self.obj(),
                    move |_, position, removed, added| {
                        if let Some(ref shuffle) = *this.imp().shuffle.borrow()
                            && let Some(shuffled_pos) = shuffle.get(position as usize)
                        {
                            this.items_changed(*shuffled_pos, removed, added);
                            return;
                        }

                        this.items_changed(position, removed, added);
                    }
                ));
            }
            self.model.replace(model);

            self.obj().notify_model();
        }
    }

    impl ListModelImpl for ShuffleListModel {
        fn item_type(&self) -> glib::Type {
            if let Some(ref model) = *self.model.borrow() {
                return model.item_type();
            }

            glib::Object::static_type()
        }

        fn n_items(&self) -> u32 {
            if let Some(ref model) = *self.model.borrow() {
                return model.n_items();
            }

            0
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if let Some(ref model) = *self.model.borrow() {
                if let Some(ref shuffle) = *self.shuffle.borrow()
                    && let Some(shuffled_pos) = shuffle.get(position as usize)
                {
                    return model.item(*shuffled_pos);
                }
                return model.item(position);
            }

            None
        }
    }
}

glib::wrapper! {
    pub struct ShuffleListModel(ObjectSubclass<imp::ShuffleListModel>)
        @implements gio::ListModel;
}

impl Default for ShuffleListModel {
    fn default() -> Self {
        Self::new(gio::ListModel::NONE)
    }
}

impl ShuffleListModel {
    pub fn new(model: Option<&impl IsA<gio::ListModel>>) -> Self {
        glib::Object::builder::<Self>()
            .property("model", model.map(|m| m.as_ref()))
            .build()
    }

    pub fn shuffled(&self) -> bool {
        self.imp().shuffle.borrow().is_some()
    }

    pub fn reshuffle(&self, anchor: u32) {
        if let Some(ref model) = *self.imp().model.borrow() {
            let n_tracks = model.n_items();
            let mut rng = rand::rng();

            let positions: Vec<u32> = if anchor == 0 {
                let mut before: Vec<u32> = vec![0];
                let mut after: Vec<u32> = (1..n_tracks).collect();
                after.shuffle(&mut rng);

                before.extend(after);
                before
            } else if anchor == n_tracks - 1 {
                let mut before: Vec<u32> = (0..n_tracks - 1).collect();
                let after: Vec<u32> = vec![n_tracks - 1];
                before.shuffle(&mut rng);

                before.extend(after);
                before
            } else {
                let mut before: Vec<u32> = (0..anchor).collect();
                let mut after: Vec<u32> = (anchor + 1..n_tracks).collect();
                after.shuffle(&mut rng);

                before.push(anchor);
                before.extend(after);
                before
            };

            self.imp().shuffle.replace(Some(positions));
            self.items_changed(0, model.n_items(), model.n_items());
        } else {
            self.imp().shuffle.replace(None);
        }
    }

    pub fn unshuffle(&self) {
        if let Some(ref model) = *self.imp().model.borrow() {
            self.imp().shuffle.replace(None);
            self.items_changed(0, model.n_items(), model.n_items());
        }
    }
}
