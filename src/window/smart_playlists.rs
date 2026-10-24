// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::play_stats;

impl Window {
    // Plays the tracks of a smart playlist in place of the queue, like
    // dropped files, with an Undo. They're read from the play stats each
    // time, so it's never the selected playlist
    pub(super) fn play_smart_playlist(&self, name: &str) {
        let query: fn() -> Vec<String> = match name {
            "recently-played" => play_stats::recently_played,
            "most-played" => play_stats::most_played,
            _ => return,
        };
        // What was heard of the track playing isn't saved until it ends,
        // so it's saved now, to be on the list
        if let Some(player) = self.player() {
            player.save_listen();
        }

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Ok(uris) = gio::spawn_blocking(query).await else {
                    return;
                };
                if uris.is_empty() {
                    win.add_toast(i18n("No tracks played yet"));
                    return;
                }
                let files = uris.iter().map(|uri| gio::File::for_uri(uri)).collect();
                win.play_in_place_of_queue(files);
            }
        ));
    }
}
