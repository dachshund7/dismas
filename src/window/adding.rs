// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// Tracks to play next, from wherever they're chosen
#[derive(Clone)]
pub(super) enum Tracks {
    // Tracks of the queue, which are moved rather than added again
    InQueue(Vec<Track>),
    // Files, like the tracks of an album shown in the pane
    Files(Vec<gio::File>),
}

impl Tracks {
    async fn files(self) -> Option<Vec<gio::File>> {
        match self {
            Tracks::InQueue(tracks) => Some(tracks.iter().map(Track::file).collect()),
            Tracks::Files(files) => Some(files),
        }
    }
}

impl Window {
    // Puts the tracks after the playing one, or after the movements of its
    // work, and after those put there before while they're still to come;
    // see `Queue::play_next_before()`
    pub(super) fn play_next(&self, tracks: Tracks) {
        // Like adding tracks, while others are read
        if self.imp().loads.get() > 0 {
            return;
        }

        if let Tracks::InQueue(tracks) = tracks {
            self.move_to_play_next(&tracks);
            return;
        }

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Some(files) = tracks.files().await else {
                    return;
                };
                win.load_into_queue(
                    files,
                    QueueMode::Add,
                    LoadOptions {
                        place: Place::Next,
                        ..Default::default()
                    },
                );
            }
        ));
    }

    // Tracks of the queue are moved, without the playing one, which
    // stays where it is
    fn move_to_play_next(&self, tracks: &[Track]) {
        let player = self.player();
        let queue = player.queue();
        let current = queue.current();
        let moved: Vec<Track> = tracks
            .iter()
            .filter(|t| Some(*t) != current.as_ref())
            .cloned()
            .collect();
        if moved.is_empty() {
            return;
        }

        let before = queue.play_next_before(&moved);
        player.move_tracks(&moved, before.as_ref());
        // They keep the order they had in the queue, whatever the order
        // they were given in
        let last = moved.iter().max_by_key(|t| queue.position(t));
        queue.set_play_next_anchor(last);
        self.store_queue();
        if let Some(first) = moved.iter().min_by_key(|t| queue.position(t)) {
            self.reveal_queue_track(first);
        }
    }
}
