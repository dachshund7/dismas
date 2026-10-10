// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::Cell;

use gtk::{gio, glib, prelude::*, subclass::prelude::*};

use crate::audio::{CoverCache, RepeatMode, ShuffleListModel, Track};

mod imp {
    use glib::{ParamSpec, ParamSpecEnum, ParamSpecObject, ParamSpecUInt, Value};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Debug)]
    pub struct Queue {
        pub model: ShuffleListModel,
        pub store: gio::ListStore,
        pub repeat_mode: Cell<RepeatMode>,
        pub current_pos: Cell<Option<u32>>,
        pub shuffled: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Queue {
        const NAME: &'static str = "AmberolQueue";
        type Type = super::Queue;

        fn new() -> Self {
            let store = gio::ListStore::new::<Track>();
            let model = ShuffleListModel::new(Some(&store));

            Self {
                store,
                model,
                repeat_mode: Cell::new(RepeatMode::default()),
                current_pos: Cell::new(None),
                shuffled: Cell::new(false),
            }
        }
    }

    impl ObjectImpl for Queue {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecObject::builder::<Track>("current")
                        .read_only()
                        .build(),
                    ParamSpecEnum::builder::<RepeatMode>("repeat-mode")
                        .read_only()
                        .build(),
                    ParamSpecUInt::builder("n-tracks").read_only().build(),
                ]
            });

            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> Value {
            match pspec.name() {
                "current" => self.obj().current_track().to_value(),
                "repeat-mode" => self.repeat_mode.get().to_value(),
                "n-tracks" => self.store.n_items().to_value(),
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct Queue(ObjectSubclass<imp::Queue>);
}

impl Default for Queue {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Queue {
    pub fn n_tracks(&self) -> u32 {
        self.imp().model.n_items()
    }

    pub fn is_empty(&self) -> bool {
        self.imp().model.n_items() == 0
    }

    pub fn model(&self) -> &gio::ListModel {
        self.imp().model.as_ref()
    }

    pub fn track_at(&self, pos: u32) -> Option<Track> {
        if let Some(track) = self.imp().model.item(pos) {
            return Some(track.downcast::<Track>().unwrap());
        }

        None
    }

    pub fn current_track(&self) -> Option<Track> {
        if let Some(pos) = self.imp().current_pos.get() {
            return self.track_at(pos);
        }

        None
    }

    pub fn set_current_track(&self, track: Option<Track>) {
        if let Some(track) = track {
            for i in 0..self.n_tracks() {
                let s = self.track_at(i).unwrap();
                if track.equals(&s) {
                    self.imp().current_pos.replace(Some(i));
                    self.notify("current");
                    return;
                }
            }
        } else {
            self.imp().current_pos.replace(None);
            self.notify("current");
        }
    }

    pub fn current_track_index(&self) -> Option<u32> {
        self.imp().current_pos.get()
    }

    pub fn add_track(&self, track: &Track) -> bool {
        if !track.equals(&Track::default()) {
            // Add track to the backing store
            self.imp().store.append(track);
            self.notify("n-tracks");
            true
        } else {
            false
        }
    }

    pub fn add_tracks(&self, tracks: &[impl IsA<glib::Object>]) {
        self.imp()
            .store
            .splice(self.imp().model.n_items(), 0, tracks);
        self.notify("n-tracks");
    }

    pub fn remove_track(&self, track: &Track) {
        let was_shuffled = self.imp().model.shuffled();
        let n_tracks = self.n_tracks();
        for pos in 0..n_tracks {
            let s = self
                .imp()
                .store
                .item(pos)
                .unwrap()
                .downcast::<Track>()
                .unwrap();
            if s.equals(track) {
                self.imp().store.remove(pos);
                break;
            }
        }

        if n_tracks != self.n_tracks() {
            if was_shuffled {
                self.imp().model.reshuffle(0);
            }
            self.notify("n-tracks");
        }

        if self.is_empty() {
            self.imp().current_pos.replace(None);
        }
    }

    pub fn clear(&self) {
        let mut cover_cache = CoverCache::global().lock().unwrap();
        cover_cache.clear();

        self.imp().current_pos.replace(None);
        self.imp().store.remove_all();
        self.notify("n-tracks");
    }

    pub fn skip_track(&self, pos: u32) -> Option<Track> {
        self.imp().current_pos.replace(Some(pos));
        self.notify("current");
        self.track_at(pos)
    }

    pub fn previous_track(&self) -> Option<Track> {
        if let Some(current_pos) = self.imp().current_pos.get() {
            if current_pos > 0 {
                let prev = current_pos - 1;
                self.imp().current_pos.replace(Some(prev));
                self.notify("current");
                return self.track_at(current_pos - 1);
            }
        }

        None
    }

    pub fn next_track(&self) -> Option<Track> {
        let store = &self.imp().model;

        let n_tracks = store.n_items();
        if n_tracks == 0 {
            return None;
        }

        let repeat_mode = self.imp().repeat_mode.get();
        if let Some(current) = self.current_track_index() {
            let next: Option<u32> = match repeat_mode {
                RepeatMode::Consecutive if current < n_tracks - 1 => Some(current + 1),
                RepeatMode::Consecutive if current == n_tracks - 1 => None,
                RepeatMode::RepeatOne => Some(current),
                RepeatMode::RepeatAll if current < n_tracks - 1 => Some(current + 1),
                RepeatMode::RepeatAll if current == n_tracks - 1 => Some(0),
                _ => None,
            };

            if let Some(next) = next {
                self.imp().current_pos.replace(Some(next));
                self.notify("current");
                self.track_at(next)
            } else {
                self.imp().current_pos.replace(None);
                self.notify("current");
                None
            }
        } else {
            // Return the first track
            self.imp().current_pos.replace(Some(0));
            self.notify("current");
            self.track_at(0)
        }
    }

    pub fn repeat_mode(&self) -> RepeatMode {
        self.imp().repeat_mode.get()
    }

    pub fn set_repeat_mode(&self, repeat_mode: RepeatMode) {
        let old_mode = self.imp().repeat_mode.replace(repeat_mode);
        if old_mode != repeat_mode {
            self.notify("repeat-mode");
        }
    }

    pub fn is_first_track(&self) -> bool {
        if let Some(current_pos) = self.imp().current_pos.get() {
            return current_pos == 0;
        }

        false
    }

    pub fn is_last_track(&self) -> bool {
        let n_items = self.imp().model.n_items();

        if let Some(current_pos) = self.imp().current_pos.get() {
            if n_items > 0 {
                return current_pos == n_items - 1;
            }
        }

        false
    }

    pub fn is_shuffled(&self) -> bool {
        self.imp().shuffled.get()
    }

    pub fn set_shuffled(&self, shuffled: bool) {
        if shuffled != self.imp().shuffled.replace(shuffled) {
            if shuffled {
                let current_pos = self.imp().current_pos.get().unwrap_or(0);
                self.imp().model.reshuffle(current_pos);
            } else {
                let current_pos = self.current_track_index().unwrap_or(0);
                let current_track = self.track_at(current_pos);
                self.imp().model.unshuffle();
                self.set_current_track(current_track);
            }
        }
    }

    pub fn select_track_at(&self, index: u32) {
        if let Some(track) = self.imp().model.item(index) {
            let track = track.downcast_ref::<Track>().unwrap();
            let is_selected = !track.selected();
            track.set_selected(is_selected);
        }
    }

    pub fn unselect_all_tracks(&self) {
        for i in 0..self.imp().store.n_items() {
            let track = self.imp().store.item(i).unwrap();
            track.downcast_ref::<Track>().unwrap().set_selected(false);
        }
    }

    pub fn n_selected_tracks(&self) -> u32 {
        let mut count = 0;
        for i in 0..self.imp().store.n_items() {
            let track = self.imp().store.item(i).unwrap();
            if track.downcast_ref::<Track>().unwrap().selected() {
                count += 1;
            }
        }

        count
    }

    pub fn contains(&self, s: &Track) -> bool {
        for i in 0..self.imp().store.n_items() {
            let track = self.imp().store.item(i).unwrap();
            if track.downcast_ref::<Track>().unwrap().equals(s) {
                return true;
            }
        }

        false
    }
}
