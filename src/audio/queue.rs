// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::Cell;

use gtk::{gio, glib, prelude::*, subclass::prelude::*};

use crate::audio::{RepeatMode, ShuffleListModel, Track};

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
        const NAME: &'static str = "DismasQueue";
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

    // The position of a track in the order the tracks were added, even
    // when shuffled; the same file added twice is two tracks
    pub fn position_in_order(&self, track: &Track) -> Option<u32> {
        self.imp()
            .store
            .iter::<Track>()
            .position(|s| s.is_ok_and(|s| s == *track))
            .map(|pos| pos as u32)
    }

    // Shows the new tags of a track that was updated: its row and the
    // section of its work are made again
    pub fn track_changed(&self, track: &Track) {
        if let Some(pos) = self.position(track) {
            self.imp().model.items_changed(pos, 1, 1);
        }
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

        // Removing a track moves the ones after it; the current track is
        // found again first, for the handlers of the number of tracks
        self.update_current_pos(current);

        if n_tracks != self.n_tracks() {
            self.notify_n_tracks();
        }
    }

    // Moves a track before another one, or to the end; the tracks are
    // moved in the order they were added, which is the one that is saved
    pub fn move_track(&self, track: &Track, before: Option<&Track>) {
        self.move_tracks(std::slice::from_ref(track), before);
    }

    // Moves tracks together before another one, or to the end, keeping
    // the order they had; if the other one is moved as well, they go
    // before the next track that isn't
    pub fn move_tracks(&self, tracks: &[Track], before: Option<&Track>) {
        let store = &self.imp().store;
        let old: Vec<Track> = store.iter::<Track>().flatten().collect();
        let moved: Vec<Track> = old.iter().filter(|t| tracks.contains(t)).cloned().collect();
        if moved.is_empty() {
            return;
        }

        let before = before.and_then(|before| {
            let from = old.iter().position(|t| t == before)?;
            old[from..].iter().find(|t| !moved.contains(t)).cloned()
        });
        let mut new: Vec<Track> = old.iter().filter(|t| !moved.contains(t)).cloned().collect();
        let to = before
            .and_then(|before| new.iter().position(|t| *t == before))
            .unwrap_or(new.len());
        new.splice(to..to, moved);

        // Only the tracks between the first and last ones that changed
        // places are replaced, in a single change, rather than removing
        // the tracks and inserting them again, so the list redraws the
        // headers of the works around them
        let Some(start) = (0..old.len()).find(|&i| old[i] != new[i]) else {
            return;
        };
        let end = (0..old.len()).rfind(|&i| old[i] != new[i]).unwrap_or(start);

        let current = self.current();
        store.splice(start as u32, (end - start + 1) as u32, &new[start..=end]);

        // Moving tracks moves the ones between their old and new places
        self.update_current_pos(current);
    }

    // The selected tracks, in the order they were added
    pub fn selected_tracks_in_order(&self) -> Vec<Track> {
        self.imp()
            .store
            .iter::<Track>()
            .flatten()
            .filter(Track::selected)
            .collect()
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

    // The covers go with the tracks, unless other tracks of their albums
    // are still around, like the tracks that replace these
    pub fn clear(&self) {
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

    // The track that `next_track` would go to, without going there
    pub fn peek_next_track(&self) -> Option<Track> {
        self.track_at(self.next_pos(self.imp().repeat_mode.get())?)
    }

    fn advance(&self, repeat_mode: RepeatMode) -> Option<Track> {
        if self.is_empty() {
            return None;
        }

        let next = self.next_pos(repeat_mode);
        self.imp().current_pos.replace(next);
        self.notify_current();
        self.track_at(next?)
    }

    // The position after the current track, or the first one if there
    // is no current track
    fn next_pos(&self, repeat_mode: RepeatMode) -> Option<u32> {
        let n_tracks = self.imp().model.n_items();
        if n_tracks == 0 {
            return None;
        }

        let Some(current) = self.current_track_index() else {
            return Some(0);
        };

        match repeat_mode {
            RepeatMode::Consecutive if current < n_tracks - 1 => Some(current + 1),
            RepeatMode::Consecutive if current == n_tracks - 1 => None,
            RepeatMode::RepeatOne => Some(current),
            RepeatMode::RepeatAll if current < n_tracks - 1 => Some(current + 1),
            RepeatMode::RepeatAll if current == n_tracks - 1 => Some(0),
            _ => None,
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
    fn move_tracks_together_in_their_order() {
        let (queue, t) = queue_of(8);

        // Spread out tracks, given out of order, go together before
        // another one, in the order they had
        queue.move_tracks(&[t[5].clone(), t[1].clone(), t[3].clone()], Some(&t[7]));
        assert_eq!(order(&queue, &t), [0, 2, 4, 6, 1, 3, 5, 7]);

        // Before one of them: before the next track that isn't moved,
        // which is where they already are
        queue.move_tracks(&[t[1].clone(), t[3].clone()], Some(&t[3]));
        assert_eq!(order(&queue, &t), [0, 2, 4, 6, 1, 3, 5, 7]);
        queue.move_tracks(&[t[1].clone(), t[3].clone()], Some(&t[1]));
        assert_eq!(order(&queue, &t), [0, 2, 4, 6, 1, 3, 5, 7]);

        // To the start, and to the end
        queue.move_tracks(&[t[6].clone(), t[7].clone()], Some(&t[0]));
        assert_eq!(order(&queue, &t), [6, 7, 0, 2, 4, 1, 3, 5]);
        queue.move_tracks(&[t[6].clone(), t[0].clone()], None);
        assert_eq!(order(&queue, &t), [7, 2, 4, 1, 3, 5, 6, 0]);
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

    #[test]
    fn remove_track_finds_the_current_track_before_notifying() {
        let (queue, t) = queue_of(3);
        queue.set_current_track(Some(t[2].clone()));

        // What the handlers of the number of tracks see
        let seen = std::rc::Rc::new(Cell::new(None));
        queue.connect_n_tracks_notify({
            let seen = seen.clone();
            move |queue| seen.set(Some(queue.current()))
        });

        queue.remove_track(&t[0]);
        assert_eq!(seen.take(), Some(Some(t[2].clone())));
        assert_eq!(queue.current_track_index(), Some(1));
    }

    #[test]
    fn peek_next_track_follows_the_repeat_mode() {
        let (queue, t) = queue_of(3);

        // Without a current track, the first one
        assert_eq!(queue.peek_next_track(), Some(t[0].clone()));
        assert_eq!(queue.current_track_index(), None);

        queue.set_current_track(Some(t[2].clone()));
        assert_eq!(queue.peek_next_track(), None);

        queue.set_repeat_mode(RepeatMode::RepeatAll);
        assert_eq!(queue.peek_next_track(), Some(t[0].clone()));

        queue.set_repeat_mode(RepeatMode::RepeatOne);
        assert_eq!(queue.peek_next_track(), Some(t[2].clone()));

        assert_eq!(queue.current_track_index(), Some(2));
    }

    #[test]
    fn peek_next_track_is_where_next_track_goes() {
        for mode in [
            RepeatMode::Consecutive,
            RepeatMode::RepeatAll,
            RepeatMode::RepeatOne,
        ] {
            let (queue, _) = queue_of(3);
            queue.set_repeat_mode(mode);
            for _ in 0..5 {
                let peeked = queue.peek_next_track();
                assert_eq!(queue.next_track(), peeked, "{mode}");
            }
        }
    }
}
