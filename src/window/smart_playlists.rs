// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::HashMap, path::PathBuf};

use adw::prelude::*;

use super::*;
use crate::{audio::shuffle, library::MusicFolder};

impl Window {
    // Plays the tracks of a smart playlist in place of the queue, like
    // dropped files, with an Undo. They're read from the play stats, or
    // the favorites, each time, so it's never the selected playlist
    pub(super) fn play_smart_playlist(&self, name: &str) {
        if name == "random-album" {
            self.play_random_album();
            return;
        }
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

    // Plays an album of the grid picked at random in place of the queue,
    // with an Undo; one not heard lately is more likely, as with the tracks
    // the shuffle puts first, and the one it played last isn't picked
    // again, so choosing it again plays another
    fn play_random_album(&self) {
        // What was heard of the track playing counts, as for the lists
        self.player().save_listen();
        let folders = library::music_folders(&self.imp().settings);
        let last = self.imp().last_random_album.borrow().clone();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let picked = gio::spawn_blocking(move || pick_album(&folders, last.as_deref()))
                    .await
                    .ok()
                    .flatten();
                let Some((album, paths)) = picked else {
                    win.add_toast(i18n("No albums found"));
                    return;
                };
                win.imp().last_random_album.replace(Some(album.id));
                let files = paths.into_iter().map(gio::File::for_path).collect();
                // Translators: `{album}` must be left unmodified; it is
                // expanded to the title of an album
                let message = i18n_k("Playing “{album}”", &[("album", &album.title)]);
                win.play_announced_in_place_of_queue(files, Some(message));
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

// An album of the grid, without the one played `last` unless it's the only
// one, picked at random by what was heard of its tracks, with the paths of
// its tracks; this blocks
fn pick_album(
    folders: &[MusicFolder],
    last: Option<&str>,
) -> Option<(library::AlbumInfo, Vec<PathBuf>)> {
    let roots = library::roots(folders);
    let hidden = [roots.unavailable, roots.excluded].concat();
    let mut tracks: HashMap<String, Vec<PathBuf>> =
        library::album_track_paths(&hidden).into_iter().collect();
    let mut albums: Vec<(library::AlbumInfo, Vec<PathBuf>)> = library::albums(&hidden)
        .into_iter()
        .filter_map(|album| {
            let paths = tracks.remove(&album.id)?;
            Some((album, paths))
        })
        .collect();
    if albums.len() > 1 {
        albums.retain(|(album, _)| Some(album.id.as_str()) != last);
    }

    let uris: Vec<Vec<String>> = albums
        .iter()
        .map(|(_, paths)| {
            paths
                .iter()
                .map(|path| gio::File::for_path(path).uri().to_string())
                .collect()
        })
        .collect();
    let stats = play_stats::track_stats(&uris.concat());
    let now = play_stats::now();
    let weights: Vec<f64> = uris
        .iter()
        .map(|uris| shuffle::weight(uris.iter().filter_map(|uri| stats.get(uri).copied()), now))
        .collect();
    let pos = shuffle::weighted_pick(&weights, &mut rand::rng())?;
    Some(albums.swap_remove(pos))
}
