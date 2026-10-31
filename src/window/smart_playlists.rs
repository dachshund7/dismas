// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::HashMap, path::PathBuf};

use adw::prelude::*;

use super::*;
use crate::{
    audio::shuffle,
    play_stats::{FavoriteAlbum, FavoriteItem, TrackStats},
};

impl Window {
    // Plays the tracks of a smart playlist in place of the queue, like
    // dropped files, with an Undo. They're read from the play stats, or
    // the favorites, each time, so it's never the selected playlist.
    // Favorite Albums plays the favorite albums whole, in the order they
    // were marked. Favorite Works plays the favorite works and recordings
    // in the order they were marked: each recording marked, and for a work
    // marked without one, a recording of it picked as Random Work picks
    // one, so that they take turns
    pub(super) fn play_smart_playlist(&self, name: &str) {
        type Query = Box<dyn FnOnce() -> Vec<String> + Send>;
        let (query, empty): (Query, _) = match name {
            "random-album" => return self.play_random(Random::Album),
            "random-work" => return self.play_random(Random::Work),
            "favorite-albums" => {
                let hidden = self.imp().library_view.hidden_folders();
                (
                    Box::new(move || uris_of(&favorite_albums_paths(&hidden))),
                    i18n("No favorite albums yet"),
                )
            }
            "favorite-works" => {
                let hidden = self.imp().library_view.hidden_folders();
                (
                    Box::new(move || uris_of(&favorite_works_paths(&hidden))),
                    i18n("No favorite works yet"),
                )
            }
            "favorites" => (
                Box::new(play_stats::favorites),
                i18n("No favorite tracks yet"),
            ),
            "recently-played" => (
                Box::new(play_stats::recently_played),
                i18n("No tracks played yet"),
            ),
            "most-played" => (
                Box::new(play_stats::most_played),
                i18n("No tracks played yet"),
            ),
            "not-heard-lately" => (
                Box::new(play_stats::not_heard_lately),
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

    // Plays an album of the grid, or a recording of a work, picked at
    // random in place of the queue, with an Undo; one not heard lately is
    // more likely, as with the tracks the shuffle puts first, and the one
    // it played last isn't picked again, so choosing it again plays another
    fn play_random(&self, random: Random) {
        // What was heard of the track playing counts, as for the lists
        self.player().save_listen();
        let hidden = self.imp().library_view.hidden_folders();
        let last = random.last(self).borrow().clone();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let picked = gio::spawn_blocking(move || random.pick(&hidden, last.as_deref()))
                    .await
                    .ok()
                    .flatten();
                let Some(picked) = picked else {
                    win.add_toast(random.none_found());
                    return;
                };
                random.last(&win).replace(Some(picked.id));
                let files = picked.paths.into_iter().map(gio::File::for_path).collect();
                let message = random.announce(&picked.name);
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

// What Random Album and Random Work, on the Playlists page, pick from
#[derive(Clone, Copy)]
enum Random {
    Album,
    Work,
}

// An album or a work that `Random` picked: its id, which it isn't picked
// by the next time, its name, and the paths of the tracks to play
struct Picked {
    id: String,
    name: String,
    paths: Vec<PathBuf>,
}

impl Random {
    // The id of the one played last
    fn last(self, win: &Window) -> &RefCell<Option<String>> {
        match self {
            Self::Album => &win.imp().last_random_album,
            Self::Work => &win.imp().last_random_work,
        }
    }

    // One picked at random, without the tracks in the `hidden` folders and
    // without the one played `last` unless it's the only one; this blocks
    fn pick(self, hidden: &[PathBuf], last: Option<&str>) -> Option<Picked> {
        match self {
            Self::Album => pick_album(hidden, last),
            Self::Work => pick_work(hidden, last),
        }
    }

    fn none_found(self) -> String {
        match self {
            Self::Album => i18n("No albums found"),
            Self::Work => i18n("No works found"),
        }
    }

    // The toast that names the one picked, as it plays
    fn announce(self, name: &str) -> String {
        match self {
            // Translators: `{album}` must be left unmodified; it is
            // expanded to the title of an album
            Self::Album => i18n_k("Playing “{album}”", &[("album", name)]),
            // Translators: `{work}` must be left unmodified; it is expanded
            // to the name of a musical work, such as “Goldberg Variations”
            Self::Work => i18n_k("Playing “{work}”", &[("work", name)]),
        }
    }
}

// An album of the grid, picked at random by what was heard of its tracks,
// with the paths of its tracks; see `Random::pick`
fn pick_album(hidden: &[PathBuf], last: Option<&str>) -> Option<Picked> {
    let mut tracks: HashMap<String, Vec<PathBuf>> =
        library::album_track_paths(hidden).into_iter().collect();
    let mut albums: Vec<(library::AlbumInfo, Vec<PathBuf>)> = library::albums(hidden)
        .into_iter()
        .filter_map(|album| {
            let paths = tracks.remove(&album.id)?;
            Some((album, paths))
        })
        .collect();
    if albums.len() > 1 {
        albums.retain(|(album, _)| Some(album.id.as_str()) != last);
    }

    let uris: Vec<Vec<String>> = albums.iter().map(|(_, paths)| uris_of(paths)).collect();
    let stats = play_stats::track_stats(&uris.concat());
    let pos = pick_heard(&uris, &stats, play_stats::now(), &mut rand::rng())?;
    let (album, paths) = albums.swap_remove(pos);
    Some(Picked {
        id: album.id,
        name: album.title,
        paths,
    })
}

// A work of the albums of the grid, picked at random by what was heard of
// all its recordings, so that a work heard lately in any of them is less
// likely, and one of its recordings, picked by what was heard of it, so
// that its recordings take turns, with the paths of its tracks; see
// `Random::pick`
fn pick_work(hidden: &[PathBuf], last: Option<&str>) -> Option<Picked> {
    let mut works = library::work_recording_paths(hidden);
    if works.len() > 1 {
        works.retain(|(work, _)| Some(work.key.as_str()) != last);
    }

    let uris: Vec<Vec<Vec<String>>> = works
        .iter()
        .map(|(_, recordings)| recordings.iter().map(|paths| uris_of(paths)).collect())
        .collect();
    let all: Vec<String> = uris.iter().flatten().flatten().cloned().collect();
    let stats = play_stats::track_stats(&all);
    let now = play_stats::now();
    let mut rng = rand::rng();
    let weights: Vec<f64> = uris
        .iter()
        .map(|recordings| heard_weight(recordings.iter().flatten(), &stats, now))
        .collect();
    let pos = shuffle::weighted_pick(&weights, &mut rng)?;
    let (work, mut recordings) = works.swap_remove(pos);

    let pos = pick_heard(&uris[pos], &stats, now, &mut rng)?;
    Some(Picked {
        id: work.key,
        name: work.name,
        paths: recordings.swap_remove(pos),
    })
}

// The weight of some tracks, by what was heard of them, see
// `shuffle::weight`
fn heard_weight<'a>(
    uris: impl Iterator<Item = &'a String> + Clone,
    stats: &HashMap<String, TrackStats>,
    now: i64,
) -> f64 {
    shuffle::weight(uris.filter_map(|uri| stats.get(uri).copied()), now)
}

// The position of one of the groups of tracks, such as albums or the
// recordings of a work, picked at random by what was heard of each
fn pick_heard(
    groups: &[Vec<String>],
    stats: &HashMap<String, TrackStats>,
    now: i64,
    rng: &mut impl rand::Rng,
) -> Option<usize> {
    let weights: Vec<f64> = groups
        .iter()
        .map(|uris| heard_weight(uris.iter(), stats, now))
        .collect();
    shuffle::weighted_pick(&weights, rng)
}

// The tracks Favorite Albums plays, outside the `hidden` folders; this
// blocks
fn favorite_albums_paths(hidden: &[PathBuf]) -> Vec<PathBuf> {
    let favorites = play_stats::favorite_albums();
    if favorites.is_empty() {
        return Vec::new();
    }
    favorite_album_paths(
        &favorites,
        library::album_track_paths(hidden).into_iter().collect(),
    )
}

// The tracks of the favorite albums, in the order they were marked, each
// album's in its order, from the `tracks` of the albums by their ids. An
// album that's gone, or in a hidden folder, gives none. One whose id
// changed with its tags was moved to its new id when the library was read
// again after the scan, see `LibraryView::reload`
fn favorite_album_paths(
    favorites: &[FavoriteAlbum],
    mut tracks: HashMap<String, Vec<PathBuf>>,
) -> Vec<PathBuf> {
    favorites
        .iter()
        .filter_map(|favorite| tracks.remove(&favorite.id))
        .flatten()
        .collect()
}

// The tracks Favorite Works plays, outside the `hidden` folders; this
// blocks
fn favorite_works_paths(hidden: &[PathBuf]) -> Vec<PathBuf> {
    let items = play_stats::favorite_items();
    if items.is_empty() {
        return Vec::new();
    }
    let recordings: HashMap<String, Vec<Vec<PathBuf>>> = library::work_recording_paths(hidden)
        .into_iter()
        .map(|(work, recordings)| (work.key, recordings))
        .collect();
    let now = play_stats::now();
    let mut rng = rand::rng();
    favorite_paths(
        &items,
        |recording| {
            library::find_recording(
                &recording.work_key,
                &recording.album_id,
                &recording.uri,
                hidden,
            )
            .map(|recording| recording.paths)
        },
        |key| recordings.get(key).map(Vec::as_slice),
        |recordings| {
            let uris: Vec<Vec<String>> = recordings.iter().map(|paths| uris_of(paths)).collect();
            let stats = play_stats::track_stats(&uris.concat());
            let pos = pick_heard(&uris, &stats, now, &mut rng)?;
            recordings.get(pos).cloned()
        },
    )
}

// The tracks of the favorites, in their order: each recording marked, as
// `find` finds it, and for a work marked, or a recording that's gone, one
// of the work's recordings, as `pick` picks it among those `recordings_of`
// gives; a work that's gone gives none
fn favorite_paths<'a>(
    items: &[FavoriteItem],
    find: impl Fn(&FavoriteRecording) -> Option<Vec<PathBuf>>,
    recordings_of: impl Fn(&str) -> Option<&'a [Vec<PathBuf>]>,
    mut pick: impl FnMut(&[Vec<PathBuf>]) -> Option<Vec<PathBuf>>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for item in items {
        let (work_key, found) = match item {
            FavoriteItem::Recording(recording) => (recording.work_key.as_str(), find(recording)),
            FavoriteItem::Work(key) => (key.as_str(), None),
        };
        let found = found.or_else(|| pick(recordings_of(work_key)?));
        paths.extend(found.into_iter().flatten());
    }
    paths
}

// The URIs of the files at the paths, as the play stats know them
fn uris_of(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| gio::File::for_path(path).uri().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{favorite_album_paths, favorite_paths};
    use crate::play_stats::{FavoriteAlbum, FavoriteItem, FavoriteRecording};

    #[test]
    fn favorite_albums_play_whole_in_the_order_they_were_marked() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        let favorite = |id: &str| FavoriteAlbum {
            id: id.to_string(),
            uri: None,
        };
        let tracks = [
            ("a", paths(&["/m/A/01", "/m/A/02"])),
            ("b", paths(&["/m/B/01"])),
            ("c", paths(&["/m/C/01"])),
        ]
        .into_iter()
        .map(|(id, paths)| (id.to_string(), paths))
        .collect();
        // Marked b, then a; "gone" left the library, or is in a hidden
        // folder, and c isn't a favorite
        let found = favorite_album_paths(&[favorite("b"), favorite("gone"), favorite("a")], tracks);
        assert_eq!(found, paths(&["/m/B/01", "/m/A/01", "/m/A/02"]));
    }

    #[test]
    fn favorites_play_their_recording_or_one_picked() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        let recording = |work_key: &str, uri: &str| FavoriteRecording {
            work_key: work_key.to_string(),
            album_id: "album".to_string(),
            uri: uri.to_string(),
        };
        let mass = vec![
            paths(&["/m/A/01", "/m/A/02"]),
            paths(&["/m/B/01", "/m/B/02"]),
        ];
        let items = [
            FavoriteItem::Recording(recording("mass", "file:///m/B/01")),
            FavoriteItem::Work("mass".to_string()),
            // Gone from the music folders
            FavoriteItem::Work("gone".to_string()),
            // Its album is gone, so one of its work's recordings plays
            FavoriteItem::Recording(recording("mass", "file:///m/C/01")),
        ];
        let found = favorite_paths(
            &items,
            |recording| (recording.uri == "file:///m/B/01").then(|| mass[1].clone()),
            |key| (key == "mass").then_some(mass.as_slice()),
            |recordings| recordings.first().cloned(),
        );
        assert_eq!(
            found,
            paths(&[
                "/m/B/01", "/m/B/02", "/m/A/01", "/m/A/02", "/m/A/01", "/m/A/02"
            ])
        );
    }
}
