// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::prelude::*;

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

    // Asks before clearing the history, from Preferences, which shows the
    // toast once it's done
    pub(super) fn confirm_clear_play_history(&self) {
        let preferences = self.visible_dialog().and_downcast::<PreferencesDialog>();
        let dialog = adw::AlertDialog::builder()
            .heading(i18n("Clear the Play History?"))
            .body(i18n(
                "Recently Played, Most Played and Not Heard Lately will be empty, \
                 and the shuffle won’t go by what you’ve heard until you play more. \
                 Your favorites are kept.",
            ))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &i18n("_Cancel")),
            ("clear", &i18n("_Clear History")),
        ]);
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        dialog.connect_response(
            Some("clear"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    let message = win.clear_play_history();
                    match &preferences {
                        Some(preferences) => preferences.toast(&message),
                        None => win.add_toast(message),
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    // What was heard of the playing track goes too, and what's heard of it
    // from now on counts
    fn clear_play_history(&self) -> String {
        self.player().forget_listen();
        match play_stats::clear_history() {
            Ok(()) => i18n("Play history cleared"),
            Err(e) => {
                log::warn!("Unable to clear the play history: {e}");
                i18n("Unable to clear the play history")
            }
        }
    }
}
