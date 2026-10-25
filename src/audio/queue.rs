// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::Cell,
    collections::{HashMap, HashSet, VecDeque},
};

use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use rand::Rng;

use crate::{
    audio::{
        RepeatMode, Track,
        shuffle::{Entry, shuffled_order},
    },
    play_stats::TrackStats,
    work_sections::continues_work,
};

mod imp {
    use super::*;

    #[derive(Debug, glib::Properties)]
    #[properties(wrapper_type = super::Queue)]
    pub struct Queue {
        pub store: gio::ListStore,
        #[property(get, builder(RepeatMode::default()))]
        pub repeat_mode: Cell<RepeatMode>,
        #[property(name = "current", get = Self::current, nullable, type = Option<Track>)]
        pub current_pos: Cell<Option<u32>>,
        #[property(name = "n-tracks", get = Self::n_tracks, type = u32)]
        n_tracks: std::marker::PhantomData<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Queue {
        const NAME: &'static str = "DismasQueue";
        type Type = super::Queue;

        fn new() -> Self {
            Self {
                store: gio::ListStore::new::<Track>(),
                repeat_mode: Cell::new(RepeatMode::default()),
                current_pos: Cell::new(None),
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
            self.store.n_items()
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
        self.imp().store.n_items() == 0
    }

    pub fn model(&self) -> &gio::ListModel {
        self.imp().store.upcast_ref()
    }

    fn tracks(&self) -> Vec<Track> {
        self.imp().store.iter::<Track>().flatten().collect()
    }

    // The files of the tracks, in the order of the queue, which is the one
    // that is saved
    pub fn files(&self) -> Vec<gio::File> {
        self.tracks().iter().map(Track::file).collect()
    }

    pub fn track_at(&self, pos: u32) -> Option<Track> {
        self.imp().store.item(pos).and_downcast()
    }

    // The position of a track in the queue, if it's in it; the same file
    // added twice is two tracks
    pub fn position(&self, track: &Track) -> Option<u32> {
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
            self.imp().store.splice(pos, 1, std::slice::from_ref(track));
        }
    }

    pub fn set_current_track(&self, track: Option<Track>) {
        if let Some(track) = track {
            if let Some(pos) = self.position(&track) {
                self.imp().current_pos.replace(Some(pos));
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
            .splice(self.imp().store.n_items(), 0, tracks);
        self.notify_n_tracks();
    }

    // Inserts tracks before another one, or at the end if it's not there
    // anymore; the current track stays
    pub fn insert_tracks(&self, tracks: &[Track], before: Option<&Track>) {
        let Some(pos) = before.and_then(|before| self.position(before)) else {
            self.add_tracks(tracks);
            return;
        };
        let current = self.current();
        self.imp().store.splice(pos, 0, tracks);
        self.notify_n_tracks();
        self.update_current_pos(current);
    }

    pub fn remove_track(&self, track: &Track) {
        self.remove_tracks(std::slice::from_ref(track));
    }

    // Removes tracks in a single change of the list, which replaces the
    // tracks from the first removed one to the last with those kept among
    // them, as `move_tracks()` does; removing them one at a time would make
    // the list, the sections of the works and the window catch up with each
    // one. Removing tracks moves the ones after them, so the current track
    // is found again first, for the handlers of the number of tracks
    pub fn remove_tracks(&self, tracks: &[Track]) {
        let removed: HashSet<&Track> = tracks.iter().collect();
        let old = self.tracks();
        let Some(start) = old.iter().position(|t| removed.contains(t)) else {
            return;
        };
        let end = old
            .iter()
            .rposition(|t| removed.contains(t))
            .unwrap_or(start);
        let kept: Vec<Track> = old[start..=end]
            .iter()
            .filter(|t| !removed.contains(t))
            .cloned()
            .collect();

        let current = self.current();
        self.imp()
            .store
            .splice(start as u32, (end - start + 1) as u32, &kept);
        self.update_current_pos(current);
        self.notify_n_tracks();
    }

    // Where playback goes when the current track is removed with others:
    // the first track after it that can play and stays, round to the start
    // when the queue repeats all of it, and even when it repeats the track
    pub fn following_except(&self, removed: &HashSet<Track>) -> Option<u32> {
        let n_tracks = self.imp().store.n_items();
        let current = self.current_track_index()?;
        let stays = |pos: u32| {
            self.playable(pos)
                && self
                    .track_at(pos)
                    .is_some_and(|track| !removed.contains(&track))
        };
        let wrap = self.imp().repeat_mode.get() == RepeatMode::RepeatAll;
        let before = if wrap { 0..current } else { 0..0 };
        (current + 1..n_tracks)
            .chain(before)
            .find(|&pos| stays(pos))
    }

    // The first track that can play and isn't removed
    pub fn first_except(&self, removed: &HashSet<Track>) -> Option<u32> {
        (0..self.imp().store.n_items()).find(|&pos| {
            self.playable(pos)
                && self
                    .track_at(pos)
                    .is_some_and(|track| !removed.contains(&track))
        })
    }

    // Moves a track before another one, or to the end
    pub fn move_track(&self, track: &Track, before: Option<&Track>) {
        self.move_tracks(std::slice::from_ref(track), before);
    }

    // Moves tracks together before another one, or to the end, keeping
    // the order they had; if the other one is moved as well, they go
    // before the next track that isn't
    pub fn move_tracks(&self, tracks: &[Track], before: Option<&Track>) {
        let store = &self.imp().store;
        let old = self.tracks();
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

    // Shuffles the tracks, in a single change, by what was heard of them,
    // from the stats of their URIs; see `shuffled_order()`. The track to
    // keep comes first, with the other movements of its work around it, and
    // all the others after it, those that came before it too
    pub fn shuffle(
        &self,
        keep: Option<&Track>,
        stats: &HashMap<String, TrackStats>,
        now: i64,
        rng: &mut (impl Rng + ?Sized),
    ) {
        let tracks = self.tracks();
        let entries: Vec<Entry> = tracks
            .iter()
            .enumerate()
            .map(|(pos, track)| Entry {
                continues_work: pos > 0 && continues_work(&tracks[pos - 1], track),
                artist: track.shuffle_artist(),
                stats: stats.get(&track.uri()).copied(),
            })
            .collect();
        let keep = keep.and_then(|keep| tracks.iter().position(|t| t == keep));
        let shuffled: Vec<Track> = shuffled_order(&entries, keep, now, rng)
            .into_iter()
            .map(|pos| tracks[pos].clone())
            .collect();
        self.replace_order(&shuffled);
    }

    // Puts the tracks back in the order of the URIs saved before they were
    // shuffled, in a single change; see `restored_order()`
    pub fn restore_order(&self, saved: &[String]) {
        let tracks = self.tracks();
        let uris: Vec<String> = tracks.iter().map(Track::uri).collect();
        let restored: Vec<Track> = restored_order(&uris, saved)
            .into_iter()
            .map(|i| tracks[i].clone())
            .collect();
        if restored != tracks {
            self.replace_order(&restored);
        }
    }

    // The same tracks in another order; the current track stays
    fn replace_order(&self, tracks: &[Track]) {
        let current = self.current();
        let store = &self.imp().store;
        store.splice(0, store.n_items(), tracks);
        self.update_current_pos(current);
    }

    // The selected tracks, in the order of the queue
    pub fn selected_tracks(&self) -> Vec<Track> {
        self.tracks().into_iter().filter(Track::selected).collect()
    }

    // The track after this one in the queue
    pub fn track_after(&self, track: &Track) -> Option<Track> {
        self.track_at(self.position(track)? + 1)
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

    // The track before the current one that can play, skipping those whose
    // files weren't found
    pub fn previous_track(&self) -> Option<Track> {
        let current_pos = self.imp().current_pos.get()?;
        let prev = (0..current_pos).rev().find(|&pos| self.playable(pos))?;
        self.imp().current_pos.replace(Some(prev));
        self.notify_current();
        self.track_at(prev)
    }

    // Whether the track at a position can play: it's there, and its file
    // was found
    fn playable(&self, pos: u32) -> bool {
        self.track_at(pos).is_some_and(|track| !track.missing())
    }

    // The first track from a position on that can play
    pub fn playable_from(&self, pos: u32) -> Option<u32> {
        (pos..self.imp().store.n_items()).find(|&pos| self.playable(pos))
    }

    pub fn next_track(&self) -> Option<Track> {
        self.advance(self.imp().repeat_mode.get())
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
    // is no current track, skipping the tracks whose files weren't found
    fn next_pos(&self, repeat_mode: RepeatMode) -> Option<u32> {
        let n_tracks = self.imp().store.n_items();
        let Some(current) = self.current_track_index() else {
            return self.playable_from(0);
        };

        match repeat_mode {
            RepeatMode::Consecutive => self.playable_from(current + 1),
            RepeatMode::RepeatOne => Some(current).filter(|&pos| self.playable(pos)),
            // Round to the start, and to the current track itself
            RepeatMode::RepeatAll => (current + 1..n_tracks)
                .chain(0..=current)
                .find(|&pos| self.playable(pos)),
        }
    }

    pub fn set_repeat_mode(&self, repeat_mode: RepeatMode) {
        let old_mode = self.imp().repeat_mode.replace(repeat_mode);
        if old_mode != repeat_mode {
            self.notify_repeat_mode();
        }
    }

    // Whether no track before the current one can play
    pub fn is_first_track(&self) -> bool {
        self.current_track_index()
            .is_some_and(|current| !(0..current).any(|pos| self.playable(pos)))
    }

    // Whether no track after the current one can play
    pub fn is_last_track(&self) -> bool {
        self.current_track_index()
            .is_some_and(|current| self.playable_from(current + 1).is_none())
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

// The order of the tracks of the queue, by their URIs, put back in the
// order saved before they were shuffled: the nth copy of a file takes the
// place of the nth one in it, the tracks that aren't in it, added since,
// go after those in the order they have, and the ones that are only in
// it, removed since, stay out. Returns the positions of the tracks
fn restored_order(current: &[String], saved: &[String]) -> Vec<usize> {
    let mut copies: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (pos, uri) in current.iter().enumerate() {
        copies.entry(uri).or_default().push_back(pos);
    }
    let mut order: Vec<usize> = saved
        .iter()
        .filter_map(|uri| copies.get_mut(uri.as_str())?.pop_front())
        .collect();

    let mut placed = vec![false; current.len()];
    for &pos in &order {
        placed[pos] = true;
    }
    order.extend((0..current.len()).filter(|&pos| !placed[pos]));
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::track::TrackData;
    use std::{cell::RefCell, rc::Rc};

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
    fn insert_tracks_before_a_track_or_at_the_end() {
        let (queue, mut t) = queue_of(3);
        let new: Vec<Track> = (0..2).map(|_| Track::new(TrackData::default())).collect();
        t.extend(new.iter().cloned());

        queue.insert_tracks(&new[..1], Some(&t[1]));
        assert_eq!(order(&queue, &t), [0, 3, 1, 2]);

        queue.insert_tracks(&new[1..], None);
        assert_eq!(order(&queue, &t), [0, 3, 1, 2, 4]);
    }

    #[test]
    fn insert_tracks_keeps_the_current_track() {
        let (queue, t) = queue_of(3);
        queue.set_current_track(Some(t[1].clone()));
        let new: Vec<Track> = (0..2).map(|_| Track::new(TrackData::default())).collect();

        queue.insert_tracks(&new, Some(&t[0]));
        assert_eq!(queue.current(), Some(t[1].clone()));
        assert_eq!(queue.current_track_index(), Some(3));
    }

    #[test]
    fn insert_tracks_before_a_removed_track_goes_at_the_end() {
        let (queue, mut t) = queue_of(3);
        queue.remove_track(&t[1]);
        let new = Track::new(TrackData::default());
        t.push(new.clone());

        queue.insert_tracks(std::slice::from_ref(&new), Some(&t[1]));
        assert_eq!(order(&queue, &t), [0, 2, 3]);
    }

    #[test]
    fn insert_tracks_is_one_change() {
        let (queue, t) = queue_of(3);
        let changes = Rc::new(RefCell::new(Vec::new()));
        queue.model().connect_items_changed({
            let changes = changes.clone();
            move |_, position, removed, added| changes.borrow_mut().push((position, removed, added))
        });
        let new: Vec<Track> = (0..2).map(|_| Track::new(TrackData::default())).collect();

        queue.insert_tracks(&new, Some(&t[1]));
        assert_eq!(*changes.borrow(), [(1, 0, 2)]);
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
    fn remove_tracks_is_one_change() {
        let (queue, t) = queue_of(6);
        let changes = Rc::new(RefCell::new(Vec::new()));
        queue.model().connect_items_changed({
            let changes = changes.clone();
            move |_, position, removed, added| changes.borrow_mut().push((position, removed, added))
        });

        // Scattered, given out of order
        queue.remove_tracks(&[t[4].clone(), t[1].clone(), t[2].clone()]);
        assert_eq!(order(&queue, &t), [0, 3, 5]);
        // From the first removed track to the last, with the one kept
        assert_eq!(*changes.borrow(), [(1, 4, 1)]);
    }

    #[test]
    fn remove_tracks_at_the_ends_and_all_of_them() {
        let (queue, t) = queue_of(4);
        queue.remove_tracks(&[t[0].clone(), t[3].clone()]);
        assert_eq!(order(&queue, &t), [1, 2]);
        // Tracks that aren't in the queue change nothing
        queue.remove_tracks(&[t[0].clone()]);
        assert_eq!(order(&queue, &t), [1, 2]);
        queue.remove_tracks(&[t[2].clone(), t[1].clone()]);
        assert!(queue.is_empty());
    }

    #[test]
    fn remove_tracks_keeps_the_current_track() {
        let (queue, t) = queue_of(5);
        queue.set_current_track(Some(t[3].clone()));
        queue.remove_tracks(&[t[0].clone(), t[2].clone()]);
        assert_eq!(queue.current(), Some(t[3].clone()));
        assert_eq!(queue.current_track_index(), Some(1));

        // The current one goes too
        queue.remove_tracks(&[t[3].clone()]);
        assert_eq!(queue.current(), None);
    }

    #[test]
    fn playback_goes_on_from_a_track_that_stays() {
        let (queue, t) = queue_of(5);
        queue.set_current_track(Some(t[1].clone()));
        let removed: HashSet<Track> = [1, 2, 4].map(|i| t[i].clone()).into();
        assert_eq!(queue.following_except(&removed), Some(3));
        assert_eq!(queue.first_except(&removed), Some(0));

        // Past the end, round to the start only when repeating
        let removed: HashSet<Track> = [1, 2, 3, 4].map(|i| t[i].clone()).into();
        assert_eq!(queue.following_except(&removed), None);
        queue.set_repeat_mode(RepeatMode::RepeatAll);
        assert_eq!(queue.following_except(&removed), Some(0));
        // Repeating the track goes on to the next one too
        queue.set_repeat_mode(RepeatMode::RepeatOne);
        let removed: HashSet<Track> = [1].map(|i| t[i].clone()).into();
        assert_eq!(queue.following_except(&removed), Some(2));
    }

    fn seeded(seed: u64) -> rand::rngs::StdRng {
        rand::SeedableRng::seed_from_u64(seed)
    }

    // The same tracks, each once
    fn is_permutation(order: &[usize], n: usize) -> bool {
        let mut sorted = order.to_vec();
        sorted.sort_unstable();
        sorted == (0..n).collect::<Vec<_>>()
    }

    #[test]
    fn shuffle_keeps_a_track_first() {
        for seed in 0..20 {
            let (queue, t) = queue_of(10);
            queue.set_current_track(Some(t[6].clone()));

            queue.shuffle(Some(&t[6]), &HashMap::new(), 0, &mut seeded(seed));
            let order = order(&queue, &t);
            assert_eq!(order[0], 6);
            assert!(is_permutation(&order, 10));
            assert_eq!(queue.current(), Some(t[6].clone()));
            assert_eq!(queue.current_track_index(), Some(0));
        }
    }

    #[test]
    fn shuffle_moves_the_tracks_before_the_current_one_too() {
        // Over a few shuffles, every track lands elsewhere at least once,
        // with or without one to keep
        for keep in [None, Some(5)] {
            let mut moved = [false; 10];
            for seed in 0..20 {
                let (queue, t) = queue_of(10);
                queue.set_current_track(Some(t[5].clone()));
                queue.shuffle(keep.map(|k| &t[k]), &HashMap::new(), 0, &mut seeded(seed));

                let order = order(&queue, &t);
                assert!(is_permutation(&order, 10));
                for (pos, &track) in order.iter().enumerate() {
                    moved[track] |= pos != track;
                }
                // The current track is found where it went
                let current = queue.current_track_index().unwrap();
                assert_eq!(order[current as usize], 5);
            }
            assert!(moved.iter().all(|&m| m), "{keep:?}: {moved:?}");
        }
    }

    #[test]
    fn shuffle_is_one_change() {
        let (queue, _) = queue_of(5);
        let changes = Rc::new(RefCell::new(Vec::new()));
        queue.model().connect_items_changed({
            let changes = changes.clone();
            move |_, position, removed, added| changes.borrow_mut().push((position, removed, added))
        });

        queue.shuffle(None, &HashMap::new(), 0, &mut seeded(1));
        assert_eq!(*changes.borrow(), [(0, 5, 5)]);
    }

    fn uris(uris: &[&str]) -> Vec<String> {
        uris.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn restore_puts_back_the_saved_order() {
        let saved = uris(&["a", "b", "c", "d"]);
        let current = uris(&["c", "a", "d", "b"]);
        assert_eq!(restored_order(&current, &saved), [1, 3, 0, 2]);
    }

    #[test]
    fn restore_puts_added_tracks_at_the_end() {
        let saved = uris(&["a", "b", "c"]);
        // Added since, in between: they go after, in the order they have
        let current = uris(&["c", "y", "a", "x", "b"]);
        assert_eq!(restored_order(&current, &saved), [2, 4, 0, 1, 3]);
    }

    #[test]
    fn restore_leaves_out_removed_tracks() {
        let saved = uris(&["a", "b", "c", "d"]);
        let current = uris(&["d", "a"]);
        assert_eq!(restored_order(&current, &saved), [1, 0]);
        assert_eq!(restored_order(&[], &saved), Vec::<usize>::new());
    }

    #[test]
    fn restore_matches_the_copies_of_a_file_in_turn() {
        let saved = uris(&["a", "b", "a", "c"]);
        let current = uris(&["a", "c", "a", "b"]);
        assert_eq!(restored_order(&current, &saved), [0, 3, 2, 1]);

        // A copy removed: the one left takes the place of the first
        let current = uris(&["c", "b", "a"]);
        assert_eq!(restored_order(&current, &saved), [2, 1, 0]);

        // A copy added: the extra one goes at the end
        let current = uris(&["a", "a", "b", "a", "c"]);
        assert_eq!(restored_order(&current, &saved), [0, 2, 1, 4, 3]);
    }

    #[test]
    fn restore_order_keeps_the_current_track() {
        let (queue, t) = queue_of(4);
        queue.set_current_track(Some(t[1].clone()));
        queue.shuffle(Some(&t[1]), &HashMap::new(), 0, &mut seeded(3));

        // All the tracks have the same URI, so the order is the one they
        // have; what matters is that nothing is lost
        let saved = vec![t[0].uri(); 4];
        queue.restore_order(&saved);
        assert!(is_permutation(&order(&queue, &t), 4));
        assert_eq!(queue.current(), Some(t[1].clone()));
    }

    // A track whose file wasn't found when its playlist loaded
    fn missing_track(name: &str) -> Track {
        let file = gtk::gio::File::for_path(format!("/gone/{name}.flac"));
        Track::new(TrackData::missing(file))
    }

    // Playing tracks at 0 and 2, and missing ones at 1 and 3
    fn queue_with_missing_tracks() -> (Queue, Vec<Track>) {
        let queue = Queue::default();
        let tracks = vec![
            Track::new(TrackData::default()),
            missing_track("b"),
            Track::new(TrackData::default()),
            missing_track("d"),
        ];
        queue.add_tracks(&tracks);
        (queue, tracks)
    }

    #[test]
    fn next_track_skips_missing_tracks() {
        let (queue, t) = queue_with_missing_tracks();
        queue.set_current_track(Some(t[0].clone()));
        assert_eq!(queue.peek_next_track(), Some(t[2].clone()));

        queue.set_current_track(Some(t[2].clone()));
        assert_eq!(queue.peek_next_track(), None);
        queue.set_repeat_mode(RepeatMode::RepeatAll);
        assert_eq!(queue.peek_next_track(), Some(t[0].clone()));
        queue.set_repeat_mode(RepeatMode::RepeatOne);
        assert_eq!(queue.peek_next_track(), Some(t[2].clone()));

        // Without a current track, the first one that can play
        let queue = Queue::default();
        queue.add_tracks(&[missing_track("a"), t[2].clone()]);
        assert_eq!(queue.peek_next_track(), Some(t[2].clone()));
    }

    #[test]
    fn a_queue_of_missing_tracks_has_nothing_to_play() {
        for mode in [
            RepeatMode::Consecutive,
            RepeatMode::RepeatAll,
            RepeatMode::RepeatOne,
        ] {
            let queue = Queue::default();
            queue.add_tracks(&[missing_track("a"), missing_track("b")]);
            queue.set_repeat_mode(mode);
            assert_eq!(queue.peek_next_track(), None, "{mode}");
            assert_eq!(queue.next_track(), None, "{mode}");
            assert_eq!(queue.playable_from(0), None, "{mode}");
        }
    }

    #[test]
    fn previous_track_skips_missing_tracks() {
        let (queue, t) = queue_with_missing_tracks();
        queue.set_current_track(Some(t[2].clone()));
        assert!(!queue.is_first_track());
        assert!(queue.is_last_track());
        assert_eq!(queue.previous_track(), Some(t[0].clone()));
        assert_eq!(queue.current_track_index(), Some(0));
        assert!(queue.is_first_track());
        assert_eq!(queue.previous_track(), None);
        assert_eq!(queue.current_track_index(), Some(0));
    }

    #[test]
    fn missing_tracks_stay_in_the_playlist() {
        let (queue, _) = queue_with_missing_tracks();
        let uris: Vec<String> = queue.files().iter().map(|f| f.uri().to_string()).collect();
        assert_eq!(uris[1], "file:///gone/b.flac");
        assert_eq!(uris.len(), 4);
        assert_eq!(queue.playable_from(1), Some(2));
        assert_eq!(queue.playable_from(3), None);
    }

    #[test]
    fn peek_next_track_is_where_next_track_goes_past_missing_tracks() {
        for mode in [
            RepeatMode::Consecutive,
            RepeatMode::RepeatAll,
            RepeatMode::RepeatOne,
        ] {
            let (queue, _) = queue_with_missing_tracks();
            queue.set_repeat_mode(mode);
            for _ in 0..5 {
                let peeked = queue.peek_next_track();
                assert_eq!(queue.next_track(), peeked, "{mode}");
            }
        }
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
