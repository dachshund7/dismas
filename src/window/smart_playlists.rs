// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // Plays the tracks of a smart playlist in place of the queue, like
    // dropped files, with an Undo. They're read from the play stats, or
    // the favorites, each time, so it's never the selected playlist
    pub(super) fn play_smart_playlist(&self, name: &str) {
        let (query, empty): (fn() -> Vec<String>, _) = match name {
            "favorites" => (play_stats::favorites, i18n("No favorite tracks yet")),
            "recently-played" => (play_stats::recently_played, i18n("No tracks played yet")),
            "most-played" => (play_stats::most_played, i18n("No tracks played yet")),
            "not-heard-lately" => (
                play_stats::not_heard_lately,
                i18n("No often played tracks have gone 2 months unheard yet"),
            ),
            _ => return,
        };
        // What was heard of the track playing isn't saved until it ends,
        // so it's saved now, to be on the list
        self.player().save_listen();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Ok(uris) = gio::spawn_blocking(query).await else {
                    return;
                };
                if uris.is_empty() {
                    win.add_toast(empty);
                    return;
                }
                let files = uris.iter().map(|uri| gio::File::for_uri(uri)).collect();
                win.play_in_place_of_queue(files);
            }
        ));
    }
}
