// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // The heart button shows whether the playing track is a favorite, and
    // is off without one
    pub(super) fn sync_track_favorite(&self) {
        let track = self.player().state().track();
        let favorite = track
            .as_ref()
            .is_some_and(|track| play_stats::is_favorite(&track.uri()));
        if favorite != self.imp().track_favorite.replace(favorite) {
            self.notify_track_favorite();
        }
        self.action_set_enabled("win.favorite", track.is_some());
    }

    // Marks any track, from its menu or with the heart button; the heart
    // button follows when it's the playing one, and so does the heart beside
    // the search with the Tracks toggle on
    pub(super) fn mark_favorite(&self, track: &Track, favorite: bool) {
        let uri = track.uri();
        play_stats::set_favorite(&uri, favorite);
        let playing = self
            .player()
            .state()
            .track()
            .is_some_and(|t| t.uri() == uri);
        if playing && favorite != self.imp().track_favorite.replace(favorite) {
            self.notify_track_favorite();
        }
        self.imp().library_view.favorite_tracks_changed();
        self.imp().queue_filter.favorites_changed();
    }

    // Marks a work as a favorite, or unmarks it; the Works page shows it
    pub(super) fn mark_favorite_work(&self, key: &str, favorite: bool) {
        play_stats::set_favorite_work(key, favorite);
        self.imp().library_view.refresh_favorites();
    }

    // Marks a recording as a favorite, or unmarks it; the Works page shows
    // it with a heart, on its work and on its cover
    pub(super) fn mark_favorite_recording(&self, recording: &FavoriteRecording, favorite: bool) {
        play_stats::set_favorite_recording(recording, favorite);
        self.imp().library_view.refresh_favorites();
    }

    // Marks albums as favorites, or unmarks them; their covers in the grid
    // show it, and so does the album pane if it shows one of them
    pub(super) fn mark_favorite_albums(&self, ids: &[String], favorite: bool) {
        play_stats::set_favorite_albums(ids, favorite);
        let imp = self.imp();
        imp.library_view.favorite_albums_changed(ids, favorite);
        imp.album_tracks.show_favorite(ids, favorite);
    }

    // The item of a menu that marks a work as a favorite or unmarks it,
    // with its action in `actions`, under `prefix`
    pub(super) fn favorite_work_item(
        &self,
        actions: &gio::SimpleActionGroup,
        prefix: &str,
        key: &str,
    ) -> gio::MenuItem {
        let favorite = play_stats::is_favorite_work(key);
        let action = gio::SimpleAction::new("favorite-work", None);
        action.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[to_owned]
            key,
            move |_, _| win.mark_favorite_work(&key, !favorite)
        ));
        actions.add_action(&action);
        let label = if favorite {
            i18n("Remove Work from Favorites")
        } else {
            i18n("Add Work to Favorites")
        };
        gio::MenuItem::new(Some(&label), Some(&format!("{prefix}.favorite-work")))
    }
}
