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
                    #[weak(rename_to = this)]
                    self.obj(),
                    move |_, position, removed, added| {
                        let (position, removed, added) = this
                            .imp()
                            .update_shuffle(position, removed, added)
                            .unwrap_or((position, removed, added));
                        this.items_changed(position, removed, added);
                    }
                ));
            }
            self.model.replace(model);

            self.obj().notify_model();
        }

        // Keeps the shuffled order in step with a change of the model, and
        // returns the change in the shuffled order, if it is shuffled. The
        // items keep their place in the shuffled order, and the new ones go
        // at the end of it
        fn update_shuffle(
            &self,
            position: u32,
            removed: u32,
            added: u32,
        ) -> Option<(u32, u32, u32)> {
            let mut shuffle = self.shuffle.borrow_mut();
            let shuffle = shuffle.as_mut()?;
            let old_len = shuffle.len() as u32;

            // Removing a single item is the only removal that is a single
            // range in the shuffled order
            let removed_at = if removed == 1 && added == 0 {
                shuffle.iter().position(|&pos| pos == position)
            } else {
                None
            };

            let end = position + removed;
            shuffle.retain(|&pos| pos < position || pos >= end);
            for pos in shuffle.iter_mut() {
                if *pos >= end {
                    *pos = *pos - removed + added;
                }
            }
            shuffle.extend(position..position + added);

            Some(match removed_at {
                Some(shuffled_pos) => (shuffled_pos as u32, 1, 0),
                None if removed == 0 => (old_len, 0, added),
                None => (0, old_len, shuffle.len() as u32),
            })
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
            .property("model", model.map(AsRef::as_ref))
            .build()
    }

    pub fn shuffled(&self) -> bool {
        self.imp().shuffle.borrow().is_some()
    }

    pub fn reshuffle(&self, anchor: u32) {
        if let Some(ref model) = *self.imp().model.borrow() {
            let n_tracks = model.n_items();
            let mut rng = rand::rng();

            let positions: Vec<u32> = if n_tracks == 0 {
                Vec::new()
            } else if anchor == 0 {
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

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gtk::{gio, glib, prelude::*};

    use super::ShuffleListModel;

    fn items(model: &impl IsA<gio::ListModel>) -> Vec<glib::Object> {
        (0..model.n_items())
            .map(|i| model.item(i).unwrap())
            .collect()
    }

    // The `items-changed` signals of a model
    type Changes = Rc<RefCell<Vec<(u32, u32, u32)>>>;

    // A shuffled model of `n` items, and the changes it reports
    fn shuffled(n: usize) -> (gio::ListStore, ShuffleListModel, Changes) {
        let store = gio::ListStore::new::<glib::Object>();
        for _ in 0..n {
            store.append(&glib::Object::new::<glib::Object>());
        }
        let model = ShuffleListModel::new(Some(&store));
        model.reshuffle(0);

        let changes = Rc::new(RefCell::new(Vec::new()));
        model.connect_items_changed({
            let changes = changes.clone();
            move |_, position, removed, added| changes.borrow_mut().push((position, removed, added))
        });

        (store, model, changes)
    }

    #[test]
    fn removing_an_item_keeps_the_shuffled_order() {
        let (store, model, changes) = shuffled(20);
        let mut order = items(&model);

        let removed = store.item(7).unwrap();
        let shuffled_pos = order.iter().position(|o| *o == removed).unwrap();
        store.remove(7);
        order.remove(shuffled_pos);

        assert_eq!(*changes.borrow(), [(shuffled_pos as u32, 1, 0)]);
        assert_eq!(items(&model), order);
    }

    #[test]
    fn added_items_go_at_the_end() {
        let (store, model, changes) = shuffled(20);
        let mut order = items(&model);

        let added = glib::Object::new::<glib::Object>();
        store.insert(3, &added);
        order.push(added);

        assert_eq!(*changes.borrow(), [(20, 0, 1)]);
        assert_eq!(items(&model), order);
    }

    #[test]
    fn removing_all_items_is_a_valid_change() {
        let (store, model, changes) = shuffled(20);
        // The last item stays last, and the ones before it are shuffled
        model.reshuffle(19);
        changes.borrow_mut().clear();

        store.remove_all();

        assert_eq!(*changes.borrow(), [(0, 20, 0)]);
        assert_eq!(model.n_items(), 0);
    }
}
