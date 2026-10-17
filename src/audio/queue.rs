// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::Cell;

use gtk::{gio, glib, prelude::*, subclass::prelude::*};

use crate::audio::{CoverCache, RepeatMode, ShuffleListModel, Track};

mod imp {
    use super::*;

    #[derive(Debug, glib::Properties)]
    #[properties(wrapper_type = super::Queue)]
    pub struct Queue {
        pub model: ShuffleListModel,
        pub store: gio::ListStore,
        #[property(get, builder(RepeatMode::default()))]
        pub repeat_mode: Cell<RepeatMode>,
        #[property(name = "current", get = Self::current, nullable, type = Option<Track>)]
        pub current_pos: Cell<Option<u32>>,
        pub shuffled: Cell<bool>,
        #[property(name = "n-tracks", get = Self::n_tracks, type = u32)]
        n_tracks: std::marker::PhantomData<u32>,
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
                n_tracks: Default::default(),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for Queue {}

    impl Queue {
        fn current(&self) -> Option<Track> {
            self.obj().track_at(self.current_pos.get()?)
        }

        fn n_tracks(&self) -> u32 {
            self.model.n_items()
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
    pub fn is_empty(&self) -> bool {
        self.imp().model.n_items() == 0
    }

    pub fn model(&self) -> &gio::ListModel {
        self.imp().model.as_ref()
    }

    // The tracks in the order they were added, even when shuffled
    pub fn files_in_order(&self) -> Vec<gio::File> {
        self.imp()
            .store
            .iter::<Track>()
            .flatten()
            .map(|track| track.file())
            .collect()
    }

    pub fn track_at(&self, pos: u32) -> Option<Track> {
        self.imp().model.item(pos).and_downcast()
    }

    // The position of a track in the queue, if it's in it
    pub fn position(&self, track: &Track) -> Option<u32> {
        self.imp()
            .model
            .iter::<Track>()
            .position(|s| s.is_ok_and(|s| s == *track))
            .map(|pos| pos as u32)
    }

    pub fn set_current_track(&self, track: Option<Track>) {
        if let Some(track) = track {
            let pos = self
                .imp()
                .model
                .iter::<Track>()
                .position(|s| s.is_ok_and(|s| s == track));
            if let Some(pos) = pos {
                self.imp().current_pos.replace(Some(pos as u32));
                self.notify_current();
            }
        } else {
            self.imp().current_pos.replace(None);
            self.notify_current();
        }
    }

    pub fn current_track_index(&self) -> Option<u32> {
        self.imp().current_pos.get()
    }

    pub fn add_track(&self, track: &Track) -> bool {
        if track.equals(&Track::default()) {
            false
        } else {
            // Add track to the backing store
            self.imp().store.append(track);
            self.notify_n_tracks();
            true
        }
    }

    pub fn add_tracks(&self, tracks: &[impl IsA<glib::Object>]) {
        self.imp()
            .store
            .splice(self.imp().model.n_items(), 0, tracks);
        self.notify_n_tracks();
    }

    pub fn remove_track(&self, track: &Track) {
        let n_tracks = self.n_tracks();
        let current = self.current();
        let pos = self
            .imp()
            .store
            .iter::<Track>()
            .position(|s| s.is_ok_and(|s| s == *track));
        if let Some(pos) = pos {
            self.imp().store.remove(pos as u32);
        }

        if n_tracks != self.n_tracks() {
            self.notify_n_tracks();
        }

        // Removing a track moves the ones after it
        self.update_current_pos(current);
    }

    // Moves a track before another one, or to the end; the tracks are
    // moved in the order they were added, which is the one that is saved
    pub fn move_track(&self, track: &Track, before: Option<&Track>) {
        if before == Some(track) {
            return;
        }
        let store = &self.imp().store;
        let store_position = |track: &Track| {
            store
                .iter::<Track>()
                .position(|s| s.is_ok_and(|s| s == *track))
                .map(|pos| pos as u32)
        };
        let Some(from) = store_position(track) else {
            return;
        };

        // Where the track ends up, once it's taken out of its old place
        let to = match before.and_then(store_position) {
            Some(before) if before > from => before - 1,
            Some(before) => before,
            None => store.n_items() - 1,
        };
        if to == from {
            return;
        }

        // The tracks between the old and new places are replaced in a
        // single change, rather than removing the track and inserting it
        // again, so the list redraws the headers of the works around them
        let (start, end) = (from.min(to), from.max(to));
        let mut tracks: Vec<Track> = (start..=end)
            .filter_map(|pos| store.item(pos).and_downcast::<Track>())
            .collect();
        if from < to {
            tracks.rotate_left(1);
        } else {
            tracks.rotate_right(1);
        }

        let current = self.current();
        store.splice(start, end - start + 1, &tracks);

        // Moving a track moves the ones between its old and new places
        self.update_current_pos(current);
    }

    // The track after this one in the order they were added, which is
    // not the shown one when shuffled
    pub fn next_in_order(&self, track: &Track) -> Option<Track> {
        let store = &self.imp().store;
        let pos = store
            .iter::<Track>()
            .position(|s| s.is_ok_and(|s| s == *track))?;
        store.item(pos as u32 + 1).and_downcast()
    }

    // Looks up the current track again, after tracks moved
    fn update_current_pos(&self, current: Option<Track>) {
        let current_pos = current.and_then(|current| self.position(&current));
        if current_pos != self.imp().current_pos.replace(current_pos) {
            self.notify_current();
        }
    }

    pub fn clear(&self) {
        let mut cover_cache = CoverCache::global().lock().unwrap();
        cover_cache.clear();
        drop(cover_cache);

        self.remove_all();
    }

    // Like `clear()`, but keeps the covers, for the tracks that replace
    // these, which can be from the same albums
    pub fn remove_all(&self) {
        self.imp().current_pos.replace(None);
        self.imp().store.remove_all();
        self.notify_n_tracks();
    }

    pub fn skip_track(&self, pos: u32) -> Option<Track> {
        self.imp().current_pos.replace(Some(pos));
        self.notify_current();
        self.track_at(pos)
    }

    pub fn previous_track(&self) -> Option<Track> {
        if let Some(current_pos) = self.imp().current_pos.get()
            && current_pos > 0
        {
            let prev = current_pos - 1;
            self.imp().current_pos.replace(Some(prev));
            self.notify_current();
            return self.track_at(current_pos - 1);
        }

        None
    }

    pub fn next_track(&self) -> Option<Track> {
        self.advance(self.imp().repeat_mode.get())
    }

    // The track after the current one, even when repeating it
    pub fn following_track(&self) -> Option<Track> {
        let repeat_mode = match self.imp().repeat_mode.get() {
            RepeatMode::RepeatOne => RepeatMode::Consecutive,
            repeat_mode => repeat_mode,
        };
        self.advance(repeat_mode)
    }

    fn advance(&self, repeat_mode: RepeatMode) -> Option<Track> {
        let store = &self.imp().model;

        let n_tracks = store.n_items();
        if n_tracks == 0 {
            return None;
        }

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
                self.notify_current();
                self.track_at(next)
            } else {
                self.imp().current_pos.replace(None);
                self.notify_current();
                None
            }
        } else {
            // Return the first track
            self.imp().current_pos.replace(Some(0));
            self.notify_current();
            self.track_at(0)
        }
    }

    pub fn set_repeat_mode(&self, repeat_mode: RepeatMode) {
        let old_mode = self.imp().repeat_mode.replace(repeat_mode);
        if old_mode != repeat_mode {
            self.notify_repeat_mode();
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

        if let Some(current_pos) = self.imp().current_pos.get()
            && n_items > 0
        {
            return current_pos == n_items - 1;
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

    pub fn unselect_all_tracks(&self) {
        for track in self.imp().store.iter::<Track>().flatten() {
            track.set_selected(false);
        }
    }

    pub fn n_selected_tracks(&self) -> u32 {
        self.imp()
            .store
            .iter::<Track>()
            .flatten()
            .filter(Track::selected)
            .count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::track::TrackData;

    fn queue_of(n: usize) -> (Queue, Vec<Track>) {
        let queue = Queue::default();
        let tracks: Vec<Track> = (0..n).map(|_| Track::new(TrackData::default())).collect();
        queue.add_tracks(&tracks);
        (queue, tracks)
    }

    fn order(queue: &Queue, tracks: &[Track]) -> Vec<usize> {
        (0..queue.n_tracks())
            .map(|i| {
                let track = queue.track_at(i).unwrap();
                tracks.iter().position(|t| *t == track).unwrap()
            })
            .collect()
    }

    #[test]
    fn move_track_forward_backward_and_to_end() {
        let (queue, t) = queue_of(4);

        queue.move_track(&t[0], Some(&t[2]));
        assert_eq!(order(&queue, &t), [1, 0, 2, 3]);

        queue.move_track(&t[3], Some(&t[1]));
        assert_eq!(order(&queue, &t), [3, 1, 0, 2]);

        queue.move_track(&t[3], None);
        assert_eq!(order(&queue, &t), [1, 0, 2, 3]);

        // Before itself, nothing moves
        queue.move_track(&t[2], Some(&t[2]));
        assert_eq!(order(&queue, &t), [1, 0, 2, 3]);
    }

    #[test]
    fn move_track_keeps_the_current_track() {
        let (queue, t) = queue_of(4);
        queue.set_current_track(Some(t[1].clone()));

        // The current track itself moves
        queue.move_track(&t[1], None);
        assert_eq!(queue.current_track_index(), Some(3));
        assert_eq!(queue.current(), Some(t[1].clone()));

        // Another track moves past it
        queue.move_track(&t[0], None);
        assert_eq!(queue.current_track_index(), Some(2));
        assert_eq!(queue.current(), Some(t[1].clone()));
    }
}
