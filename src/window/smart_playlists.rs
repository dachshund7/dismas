// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use adw::prelude::*;

use super::*;
use crate::{
    audio::shuffle,
    library::WorkCandidate,
    library_view::RandomPool,
    play_stats::{FavoriteRecording, TrackStats},
};

impl Window {
    // Plays the tracks of a smart playlist in place of the queue, like
    // dropped files, with an Undo. They're read from the play stats each
    // time, so it's never the selected playlist
    pub(super) fn play_smart_playlist(&self, name: &str) {
        type Query = Box<dyn FnOnce() -> Vec<String> + Send>;
        let (query, empty): (Query, _) = match name {
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

    // Play a Random …, in the album pages' menu: plays an album, a track or
    // a recording of a work, picked at random from what the page shows, in
    // place of the queue, with an Undo. One not heard lately is more
    // likely, as with the tracks the shuffle puts first, and the one it
    // played last on the page isn't picked again, so choosing it again
    // plays another
    pub(super) fn play_random(&self) {
        // What was heard of the track playing counts, as for the lists
        self.player().save_listen();
        let view = &self.imp().library_view;
        let pool = view.random_pool();
        let random = Random::of(&pool);
        let hidden = view.hidden_folders();
        let last = random.last(self).borrow().clone();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let picked = gio::spawn_blocking(move || pick(pool, &hidden, last.as_deref()))
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

// What Play a Random … picks on the page it's chosen on
#[derive(Clone, Copy)]
enum Random {
    Album,
    Track,
    Work,
}

// An album, a track or a work that `Random` picked: its id, which it isn't
// picked by the next time, its name, and the paths of the tracks to play
struct Picked {
    id: String,
    name: String,
    paths: Vec<PathBuf>,
}

impl Random {
    fn of(pool: &RandomPool) -> Self {
        match pool {
            RandomPool::Albums(_) => Self::Album,
            RandomPool::Tracks(_) => Self::Track,
            RandomPool::Works { .. } => Self::Work,
        }
    }

    // The id of the one played last
    fn last(self, win: &Window) -> &RefCell<Option<String>> {
        let imp = win.imp();
        match self {
            Self::Album => &imp.last_random_album,
            Self::Track => &imp.last_random_track,
            Self::Work => &imp.last_random_work,
        }
    }

    fn none_found(self) -> String {
        match self {
            Self::Album => i18n("No albums found"),
            Self::Track => i18n("No tracks found"),
            Self::Work => i18n("No works found"),
        }
    }

    // The toast that names the one picked, as it plays
    fn announce(self, name: &str) -> String {
        match self {
            // Translators: `{album}` must be left unmodified; it is
            // expanded to the title of an album
            Self::Album => i18n_k("Playing “{album}”", &[("album", name)]),
            // Translators: `{track}` must be left unmodified; it is
            // expanded to the title of a track
            Self::Track => i18n_k("Playing “{track}”", &[("track", name)]),
            // Translators: `{work}` must be left unmodified; it is expanded
            // to the name of a musical work, such as “Goldberg Variations”
            Self::Work => i18n_k("Playing “{work}”", &[("work", name)]),
        }
    }
}

// One picked at random from the pool, without the tracks in the `hidden`
// folders and without the one played `last` unless it's the only one;
// this blocks
fn pick(pool: RandomPool, hidden: &[PathBuf], last: Option<&str>) -> Option<Picked> {
    let now = play_stats::now();
    let mut rng = rand::rng();
    match pool {
        RandomPool::Albums(filter) => pick_album(&filter, hidden, last, now, &mut rng),
        RandomPool::Tracks(filter) => pick_track(&filter, hidden, last, now, &mut rng),
        RandomPool::Works {
            filter,
            favorites_only,
        } => pick_work(&filter, favorites_only, hidden, last, now, &mut rng),
    }
}

// An album with a track the filter matches, by what was heard of its
// tracks, with the paths of all its tracks
fn pick_album(
    filter: &TrackFilter,
    hidden: &[PathBuf],
    last: Option<&str>,
    now: i64,
    rng: &mut impl rand::Rng,
) -> Option<Picked> {
    let shown = (!filter.is_empty()).then(|| library::search_albums(filter, hidden));
    let mut tracks: HashMap<String, Vec<PathBuf>> =
        library::album_track_paths(hidden).into_iter().collect();
    let mut albums: Vec<(library::AlbumInfo, Vec<PathBuf>)> = library::albums(hidden)
        .into_iter()
        .filter(|album| {
            shown
                .as_ref()
                .is_none_or(|shown| shown.contains_key(&album.id))
        })
        .filter_map(|album| {
            let paths = tracks.remove(&album.id)?;
            Some((album, paths))
        })
        .collect();

    let uris: Vec<Vec<String>> = albums.iter().map(|(_, paths)| uris_of(paths)).collect();
    let stats = play_stats::track_stats(&uris.concat());
    let weights: Vec<f64> = uris
        .iter()
        .map(|uris| heard_weight(uris, &stats, now))
        .collect();
    let ids: Vec<&str> = albums.iter().map(|(album, _)| album.id.as_str()).collect();
    let pos = pick_index(&ids, &weights, last, rng)?;
    let (album, paths) = albums.swap_remove(pos);
    Some(Picked {
        id: album.id,
        name: album.title,
        paths,
    })
}

// A track the filter matches, by what was heard of it
fn pick_track(
    filter: &TrackFilter,
    hidden: &[PathBuf],
    last: Option<&str>,
    now: i64,
    rng: &mut impl rand::Rng,
) -> Option<Picked> {
    let mut tracks = library::matching_tracks(filter, hidden);
    let mut uris: Vec<String> = tracks.iter().map(|(path, _)| uri_of(path)).collect();
    let stats = play_stats::track_stats(&uris);
    let weights: Vec<f64> = uris
        .iter()
        .map(|uri| heard_weight(std::slice::from_ref(uri), &stats, now))
        .collect();
    let pos = pick_index(&uris, &weights, last, rng)?;
    let (path, title) = tracks.swap_remove(pos);
    Some(Picked {
        id: uris.swap_remove(pos),
        name: title.unwrap_or_else(|| crate::audio::title_from_file_name(&path)),
        paths: vec![path],
    })
}

// A work listed for the filter, and one of its recordings that the filter
// matches, see `choose_work()`, with the paths of the recording's tracks
fn pick_work(
    filter: &TrackFilter,
    favorites_only: bool,
    hidden: &[PathBuf],
    last: Option<&str>,
    now: i64,
    rng: &mut impl rand::Rng,
) -> Option<Picked> {
    let mut works = library::work_recording_candidates(filter, hidden);
    // The favorites as the Works page counts them, see `update_works()`
    let favorites = favorites_only.then(|| Favorites {
        recordings: play_stats::favorite_recordings(),
        works: play_stats::favorite_work_keys(),
    });
    if let Some(favorites) = &favorites {
        works.retain(|work| work.info.is_among(&favorites.works));
    }

    let paths: Vec<&Path> = works
        .iter()
        .flat_map(|work| &work.paths)
        .map(PathBuf::as_path)
        .collect();
    let uris: Vec<String> = paths.iter().map(|path| uri_of(path)).collect();
    let stats = play_stats::track_stats(&uris);
    let heard: HashMap<&Path, TrackStats> = paths
        .into_iter()
        .zip(&uris)
        .filter_map(|(path, uri)| Some((path, *stats.get(uri)?)))
        .collect();

    let (pos, recording) = choose_work(&works, &heard, favorites.as_ref(), last, now, rng)?;
    let mut work = works.swap_remove(pos);
    Some(Picked {
        paths: work.recordings.swap_remove(recording).paths,
        id: work.info.key,
        name: work.info.name,
    })
}

// The favorite works and recordings, for Play a Random Work with the heart
// on
struct Favorites {
    recordings: HashSet<FavoriteRecording>,
    // The keys of the works marked, and of those of the favorite
    // recordings, see `play_stats::favorite_work_keys()`
    works: HashSet<String>,
}

// The positions of a work and of one of its recordings, picked at random:
// the work by what was heard of all its recordings, so that a work heard
// lately in any of them is less likely, and then the recording by what was
// heard of it, so that the recordings of a work take turns. With the
// `favorites`, the recording is one of the work's favorite recordings, if
// it has any, or else one of a work marked, itself or an arrangement
// listed under it
fn choose_work(
    works: &[WorkCandidate],
    heard: &HashMap<&Path, TrackStats>,
    favorites: Option<&Favorites>,
    last: Option<&str>,
    now: i64,
    rng: &mut impl rand::Rng,
) -> Option<(usize, usize)> {
    let weight = |paths: &[PathBuf]| {
        shuffle::weight(
            paths
                .iter()
                .filter_map(|path| heard.get(path.as_path()).copied()),
            now,
        )
    };
    let keys: Vec<&str> = works.iter().map(|work| work.info.key.as_str()).collect();
    let weights: Vec<f64> = works.iter().map(|work| weight(&work.paths)).collect();
    let pos = pick_index(&keys, &weights, last, rng)?;

    let recordings = &works[pos].recordings;
    let mut chosen: Vec<usize> = (0..recordings.len()).collect();
    if let Some(favorites) = favorites {
        let favorite = |i: &usize| {
            let recording = &recordings[*i];
            recording.paths.first().is_some_and(|first| {
                favorites.recordings.contains(&FavoriteRecording {
                    work_key: recording.work_key.clone(),
                    album_id: recording.album_id.clone(),
                    uri: uri_of(first),
                })
            })
        };
        let marked = |i: &usize| favorites.works.contains(&recordings[*i].work_key);
        for preferred in [&favorite as &dyn Fn(&usize) -> bool, &marked] {
            let kept: Vec<usize> = chosen.iter().copied().filter(preferred).collect();
            if !kept.is_empty() {
                chosen = kept;
                break;
            }
        }
    }
    let weights: Vec<f64> = chosen
        .iter()
        .map(|&i| weight(&recordings[i].paths))
        .collect();
    let recording = shuffle::weighted_pick(&weights, rng)?;
    Some((pos, chosen[recording]))
}

// The weight of some tracks, by what was heard of them, see
// `shuffle::weight`
fn heard_weight(uris: &[String], stats: &HashMap<String, TrackStats>, now: i64) -> f64 {
    shuffle::weight(uris.iter().filter_map(|uri| stats.get(uri).copied()), now)
}

// The position of one of the candidates, picked at random by their
// weights, leaving out the one picked `last`, by its id, unless it's the
// only one
fn pick_index(
    ids: &[impl AsRef<str>],
    weights: &[f64],
    last: Option<&str>,
    rng: &mut impl rand::Rng,
) -> Option<usize> {
    let allowed: Vec<usize> = (0..ids.len())
        .filter(|&i| ids.len() == 1 || Some(ids[i].as_ref()) != last)
        .collect();
    let weights: Vec<f64> = allowed.iter().map(|&i| weights[i]).collect();
    shuffle::weighted_pick(&weights, rng).map(|pos| allowed[pos])
}

// The URI of the file at a path, as the play stats know it
fn uri_of(path: &Path) -> String {
    gio::File::for_path(path).uri().to_string()
}

// The URIs of the files at the paths
fn uris_of(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| uri_of(path)).collect()
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;
    use crate::library::{RecordingCandidate, WorkInfo};

    const NOW: i64 = 1_800_000_000;

    fn recording(album: &str, work_key: &str) -> RecordingCandidate {
        RecordingCandidate {
            album_id: album.to_string(),
            work_key: work_key.to_string(),
            paths: vec![PathBuf::from(format!("/m/{album}/01.flac"))],
        }
    }

    fn work(key: &str, recordings: Vec<RecordingCandidate>) -> WorkCandidate {
        WorkCandidate {
            paths: recordings
                .iter()
                .flat_map(|recording| recording.paths.clone())
                .collect(),
            info: WorkInfo {
                key: key.to_string(),
                name: key.to_string(),
                title: None,
                collection: None,
                arrangement: None,
                arrangements: Vec::new(),
                composers: Vec::new(),
                recordings: u32::try_from(recordings.len()).unwrap(),
                favorite: false,
            },
            recordings,
        }
    }

    #[test]
    fn nothing_to_pick_picks_nothing() {
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(pick_index(&[] as &[&str], &[], None, &mut rng), None);
        assert_eq!(
            choose_work(&[], &HashMap::new(), None, None, NOW, &mut rng),
            None
        );
    }

    #[test]
    fn the_last_pick_is_left_out_unless_it_is_the_only_one() {
        let mut rng = StdRng::seed_from_u64(2);
        for _ in 0..100 {
            let pick = pick_index(&["a", "b", "c"], &[1.0; 3], Some("b"), &mut rng);
            assert_ne!(pick, Some(1));
        }
        assert_eq!(pick_index(&["a"], &[1.0], Some("a"), &mut rng), Some(0));

        let works = [
            work("w", vec![recording("a", "w")]),
            work("v", vec![recording("b", "v")]),
        ];
        for _ in 0..100 {
            let pick = choose_work(&works, &HashMap::new(), None, Some("w"), NOW, &mut rng);
            assert_eq!(pick, Some((1, 0)));
        }
    }

    #[test]
    fn with_the_heart_a_favorite_recording_comes_first() {
        // A work with two recordings of its own and one of an arrangement
        // listed under it
        let works = [work(
            "w",
            vec![
                recording("a", "w"),
                recording("b", "arr"),
                recording("c", "w"),
            ],
        )];
        let favorites = |recordings: &[&RecordingCandidate], works: &[&str]| Favorites {
            recordings: recordings
                .iter()
                .map(|recording| FavoriteRecording {
                    work_key: recording.work_key.clone(),
                    album_id: recording.album_id.clone(),
                    uri: uri_of(&recording.paths[0]),
                })
                .collect(),
            works: works.iter().map(ToString::to_string).collect(),
        };
        let picks = |favorites: Option<&Favorites>| {
            let mut rng = StdRng::seed_from_u64(3);
            (0..200)
                .map(|_| {
                    choose_work(&works, &HashMap::new(), favorites, None, NOW, &mut rng)
                        .unwrap()
                        .1
                })
                .collect::<HashSet<usize>>()
        };

        // Without the heart, any of them
        assert_eq!(picks(None), HashSet::from([0, 1, 2]));
        // The favorite recording, whatever else is marked
        let own = &works[0].recordings;
        assert_eq!(
            picks(Some(&favorites(&[&own[2]], &["w", "arr"]))),
            HashSet::from([2])
        );
        // Without one, the recordings of the work marked: the arrangement,
        // which made the work it's listed under a favorite
        assert_eq!(picks(Some(&favorites(&[], &["arr"]))), HashSet::from([1]));
        // or the work itself
        assert_eq!(picks(Some(&favorites(&[], &["w"]))), HashSet::from([0, 2]));
    }
}
