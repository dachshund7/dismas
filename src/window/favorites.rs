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

    // Marks any track, e.g. from its menu; the heart button follows when
    // it's the playing one
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
    }

    // Marks a work as a favorite, or unmarks it; the Works page shows it
    pub(super) fn mark_favorite_work(&self, key: &str, favorite: bool) {
        play_stats::set_favorite_work(key, favorite);
        self.imp().library_view.refresh_favorites();
    }

    pub(super) fn mark_favorite_recording(&self, recording: &FavoriteRecording, favorite: bool) {
        play_stats::set_favorite_recording(recording, favorite);
        self.imp().library_view.refresh_favorites();
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

    // Adds to the end of a section of the menu of a movement's work the
    // item that marks the work, once the library says which work it is,
    // in a separate thread; nothing is added for a track not in it
    pub(super) fn add_favorite_work_item(
        &self,
        section: &gio::Menu,
        actions: &gio::SimpleActionGroup,
        prefix: &str,
        track: &Track,
    ) {
        let Some(path) = track.file().path() else {
            return;
        };
        let hidden = self.imp().library_view.hidden_folders();
        let position = section.n_items();
        let prefix = prefix.to_string();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            #[weak]
            section,
            #[weak]
            actions,
            async move {
                let found = gio::spawn_blocking(move || library::track_work(&path, &hidden))
                    .await
                    .ok()
                    .flatten();
                if let Some((work, _)) = found {
                    let item = win.favorite_work_item(&actions, &prefix, &work.key);
                    section.insert_item(position, &item);
                }
            }
        ));
    }
}
