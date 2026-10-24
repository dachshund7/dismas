// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // The heart button shows whether the playing track is a favorite, and
    // is off without one
    pub(super) fn sync_track_favorite(&self) {
        let track = self.player().and_then(|p| p.state().track());
        let favorite = track
            .as_ref()
            .is_some_and(|track| play_stats::is_favorite(&track.uri()));
        if favorite != self.imp().track_favorite.replace(favorite) {
            self.notify_track_favorite();
        }
        self.action_set_enabled("win.favorite", track.is_some());
    }

    // Marks any track, e.g. from its menu; the heart button follows when
    // it's the playing one
    pub(super) fn mark_favorite(&self, track: &Track, favorite: bool) {
        let uri = track.uri();
        play_stats::set_favorite(&uri, favorite);
        let playing = self
            .player()
            .and_then(|p| p.state().track())
            .is_some_and(|t| t.uri() == uri);
        if playing && favorite != self.imp().track_favorite.replace(favorite) {
            self.notify_track_favorite();
        }
    }
}
