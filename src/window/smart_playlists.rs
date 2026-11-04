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
    library::{RecordingCandidate, WorkCandidate},
    library_view::{LibraryItems, RandomPool},
    play_stats::{FavoriteRecording, TrackStats},
};

impl Window {
    // The tracks of a smart playlist, read from the play stats each time,
    // in a separate thread; with none, a toast says why. What was heard of
    // the track playing isn't saved until it ends, so it's saved now, to be
    // on the list
    pub(super) async fn smart_playlist_files(&self, kind: SmartPlaylist) -> Option<Vec<gio::File>> {
        self.player().save_listen();
        let query = match kind {
            SmartPlaylist::RecentlyPlayed => play_stats::recently_played,
            SmartPlaylist::MostPlayed => play_stats::most_played,
            SmartPlaylist::NotHeardLately => play_stats::not_heard_lately,
        };
        let uris = gio::spawn_blocking(query).await.ok()?;
        if uris.is_empty() {
            self.add_toast(kind.empty_message());
            return None;
        }
        Some(uris.iter().map(|uri| gio::File::for_uri(uri)).collect())
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
        let items = pool.items;
        let hidden = view.hidden_folders();
        let last = last_random(self, items).borrow().clone();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let picked = gio::spawn_blocking(move || pick(pool, &hidden, last.as_deref()))
                    .await
                    .ok()
                    .flatten();
                let Some(picked) = picked else {
                    win.add_toast(none_found(items));
                    return;
                };
                let message = announce(items, &picked.name, picked.part.as_ref());
                last_random(&win, items).replace(Some(picked.id));
                let files = picked.paths.into_iter().map(gio::File::for_path).collect();
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

// The smart playlists, made of the tracks the play stats pick when they're
// shown. They play a snapshot in place of the queue, so they're never the
// selected playlist
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmartPlaylist {
    RecentlyPlayed,
    MostPlayed,
    NotHeardLately,
}

impl SmartPlaylist {
    pub(crate) const ALL: [Self; 3] = [
        SmartPlaylist::RecentlyPlayed,
        SmartPlaylist::MostPlayed,
        SmartPlaylist::NotHeardLately,
    ];

    // Its name, as the target of the action that shows it
    pub(crate) fn name(self) -> &'static str {
        match self {
            SmartPlaylist::RecentlyPlayed => "recently-played",
            SmartPlaylist::MostPlayed => "most-played",
            SmartPlaylist::NotHeardLately => "not-heard-lately",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    pub(crate) fn title(self) -> String {
        match self {
            SmartPlaylist::RecentlyPlayed => i18n("Recently Played"),
            SmartPlaylist::MostPlayed => i18n("Most Played"),
            SmartPlaylist::NotHeardLately => i18n("Not Heard Lately"),
        }
    }

    // What it picks
    pub(crate) fn description(self) -> String {
        match self {
            SmartPlaylist::RecentlyPlayed => i18n("The last 100 tracks played"),
            SmartPlaylist::MostPlayed => i18n("The tracks played the most in the last 6 months"),
            SmartPlaylist::NotHeardLately => {
                i18n("The tracks played the most, but not heard in the last 2 months")
            }
        }
    }

    // Why it has no tracks
    fn empty_message(self) -> String {
        match self {
            SmartPlaylist::RecentlyPlayed | SmartPlaylist::MostPlayed => {
                i18n("No tracks played yet")
            }
            SmartPlaylist::NotHeardLately => {
                i18n("No often played tracks have gone 2 months unheard yet")
            }
        }
    }
}

// An album, a track or a work that Play a Random … picked: its id, which it isn't
// picked by the next time, its name, and the paths of the tracks to play
struct Picked {
    id: String,
    name: String,
    // For a work, what of it the recording is, if it isn't the work itself
    part: Option<Part>,
    paths: Vec<PathBuf>,
}

// What a recording of a work is, when it isn't the work itself, which the
// toast says
#[derive(Debug, PartialEq, Eq)]
enum Part {
    // A single movement of it, such as an aria of an opera on a recital
    Movement,
    // An arrangement listed under it, and what that is
    Arrangement(String),
}

impl Part {
    // What of its work a recording is, a single movement first, as a
    // movement of an arrangement is still only part of the work
    fn of(recording: &RecordingCandidate) -> Option<Self> {
        if recording.single_movement {
            return Some(Self::Movement);
        }
        let arrangement = recording.arrangement.as_ref()?;
        Some(Self::Arrangement(arrangement.shown()))
    }
}

// The id of the one Play a Random … played last for `items`
fn last_random(win: &Window, items: LibraryItems) -> &RefCell<Option<String>> {
    let imp = win.imp();
    match items {
        LibraryItems::Albums => &imp.last_random_album,
        LibraryItems::Tracks => &imp.last_random_track,
        LibraryItems::Works => &imp.last_random_work,
    }
}

fn none_found(items: LibraryItems) -> String {
    match items {
        LibraryItems::Albums => i18n("No albums found"),
        LibraryItems::Tracks => i18n("No tracks found"),
        LibraryItems::Works => i18n("No works found"),
    }
}

// The toast that names the one picked, as it plays
fn announce(items: LibraryItems, name: &str, part: Option<&Part>) -> String {
    match (items, part) {
        // Translators: `{album}` must be left unmodified; it is
        // expanded to the title of an album
        (LibraryItems::Albums, _) => i18n_k("Playing “{album}”", &[("album", name)]),
        // Translators: `{track}` must be left unmodified; it is
        // expanded to the title of a track
        (LibraryItems::Tracks, _) => i18n_k("Playing “{track}”", &[("track", name)]),
        // Translators: `{work}` must be left unmodified; it is expanded
        // to the name of a musical work, such as “Goldberg Variations”
        (LibraryItems::Works, None) => i18n_k("Playing “{work}”", &[("work", name)]),
        // Translators: `{work}` must be left unmodified; it is expanded
        // to the name of a musical work, such as “Don Giovanni”, of
        // which a single movement plays, such as an aria
        (LibraryItems::Works, Some(Part::Movement)) => {
            i18n_k("Playing part of “{work}”", &[("work", name)])
        }
        // Translators: `{work}` and `{arrangement}` must be left
        // unmodified; they are expanded to the name of a musical work and
        // to what the arrangement of it that plays is, such as “for
        // piano, Busoni”, or “arrangement”
        (LibraryItems::Works, Some(Part::Arrangement(arrangement))) => i18n_k(
            "Playing “{work}”, {arrangement}",
            &[("work", name), ("arrangement", arrangement)],
        ),
    }
}

// One picked at random from the pool, without the tracks in the `hidden`
// folders and without the one played `last` unless it's the only one;
// this blocks
fn pick(pool: RandomPool, hidden: &[PathBuf], last: Option<&str>) -> Option<Picked> {
    let now = play_stats::now();
    let mut rng = rand::rng();
    let RandomPool {
        items,
        filter,
        favorites_only,
    } = pool;
    match items {
        LibraryItems::Albums => pick_album(&filter, hidden, last, now, &mut rng),
        LibraryItems::Tracks => pick_track(&filter, hidden, last, now, &mut rng),
        LibraryItems::Works => pick_work(&filter, favorites_only, hidden, last, now, &mut rng),
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
    let mut albums: Vec<(String, Vec<PathBuf>)> = library::album_track_paths(hidden)
        .into_iter()
        .filter(|(id, _)| shown.as_ref().is_none_or(|shown| shown.contains_key(id)))
        .collect();

    let stats = play_stats::all_track_stats();
    let weights: Vec<f64> = albums
        .iter()
        .map(|(_, paths)| heard_weight(&uris_of(paths), &stats, now))
        .collect();
    let ids: Vec<&str> = albums.iter().map(|(id, _)| id.as_str()).collect();
    let pos = pick_index(&ids, &weights, last, rng)?;
    let (id, paths) = albums.swap_remove(pos);
    Some(Picked {
        name: library::album_title(&id, hidden).unwrap_or_default(),
        id,
        part: None,
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
    let stats = play_stats::all_track_stats();
    let weights: Vec<f64> = uris
        .iter()
        .map(|uri| heard_weight(std::slice::from_ref(uri), &stats, now))
        .collect();
    let pos = pick_index(&uris, &weights, last, rng)?;
    let (path, title) = tracks.swap_remove(pos);
    Some(Picked {
        id: uris.swap_remove(pos),
        name: title.unwrap_or_else(|| crate::audio::title_from_file_name(&path)),
        part: None,
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
    let stats = play_stats::all_track_stats();
    let heard: HashMap<&Path, TrackStats> = paths
        .into_iter()
        .zip(&uris)
        .filter_map(|(path, uri)| Some((path, *stats.get(uri)?)))
        .collect();

    let (pos, recording) = choose_work(&works, &heard, favorites.as_ref(), last, now, rng)?;
    let mut work = works.swap_remove(pos);
    let recording = work.recordings.swap_remove(recording);
    Some(Picked {
        part: Part::of(&recording),
        paths: recording.paths,
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
// heard of it, so that the recordings of a work take turns. Both are
// weighed by their length too, the work by that of its longest recording,
// so that short pieces don't crowd out the rest, and the music doesn't
// stop after a few minutes most of the time; a whole recording comes up
// more often than an excerpt of the same work. With the `favorites`, the
// recording is one of the work's favorite recordings, if it has any, or
// else one of a work marked, itself or an arrangement listed under it
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
    let median = median_length(works);
    let length = |recording: &RecordingCandidate| counted_length(recording, median);
    let keys: Vec<&str> = works.iter().map(|work| work.info.key.as_str()).collect();
    let weights: Vec<f64> = works
        .iter()
        .map(|work| {
            let longest = work.recordings.iter().map(length).fold(1.0, f64::max);
            weight(&work.paths) * longest
        })
        .collect();
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
        .map(|&i| weight(&recordings[i].paths) * length(&recordings[i]))
        .collect();
    let recording = shuffle::weighted_pick(&weights, rng)?;
    Some((pos, chosen[recording]))
}

// The median of the lengths of the recordings that are known, which those
// whose length isn't known count as
fn median_length(works: &[WorkCandidate]) -> Option<u64> {
    let mut known: Vec<u64> = works
        .iter()
        .flat_map(|work| &work.recordings)
        .filter_map(|recording| recording.length)
        .collect();
    known.sort_unstable();
    known.get(known.len() / 2).copied()
}

// The length a recording is weighed by, in seconds: its own, or else the
// `median` of those known, as a weight of 0 would never be picked; when no
// length is known, they all count the same
fn counted_length(recording: &RecordingCandidate, median: Option<u64>) -> f64 {
    median.map_or(1.0, |median| {
        recording.length.unwrap_or(median).max(1) as f64
    })
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
    use crate::library::WorkInfo;

    const NOW: i64 = 1_800_000_000;

    fn recording(album: &str, work_key: &str) -> RecordingCandidate {
        RecordingCandidate {
            album_id: album.to_string(),
            work_key: work_key.to_string(),
            paths: vec![PathBuf::from(format!("/m/{album}/01.flac"))],
            length: Some(600),
            single_movement: false,
            arrangement: None,
        }
    }

    fn lasting(length: Option<u64>, recording: RecordingCandidate) -> RecordingCandidate {
        RecordingCandidate {
            length,
            ..recording
        }
    }

    // How often each recording of each work is picked out of 2,000
    fn picks(works: &[WorkCandidate], seed: u64) -> HashMap<(usize, usize), u32> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut picks = HashMap::new();
        for _ in 0..2000 {
            let pick = choose_work(works, &HashMap::new(), None, None, NOW, &mut rng).unwrap();
            *picks.entry(pick).or_default() += 1;
        }
        picks
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
    fn smart_playlists_are_found_by_their_names() {
        for kind in SmartPlaylist::ALL {
            assert_eq!(SmartPlaylist::named(kind.name()), Some(kind));
        }
        assert_eq!(SmartPlaylist::named("favorites"), None);
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

    #[test]
    fn a_long_work_is_more_likely_than_a_short_one() {
        // 40 minutes and 4 minutes
        let works = [
            work(
                "symphony",
                vec![lasting(Some(2400), recording("a", "symphony"))],
            ),
            work("song", vec![lasting(Some(240), recording("b", "song"))]),
        ];
        let symphony = f64::from(picks(&works, 4)[&(0, 0)]) / 2000.0;
        assert!((0.88..0.94).contains(&symphony), "{symphony}");

        // An hour, and a five-minute excerpt of the same work
        let works = [work(
            "opera",
            vec![
                lasting(Some(3600), recording("a", "opera")),
                lasting(Some(300), recording("b", "opera")),
            ],
        )];
        let whole = f64::from(picks(&works, 5)[&(0, 0)]) / 2000.0;
        assert!((0.89..0.95).contains(&whole), "{whole}");
    }

    #[test]
    fn a_length_that_is_not_known_counts_as_the_median() {
        let works = [
            work("a", vec![lasting(Some(100), recording("a", "a"))]),
            work("b", vec![lasting(Some(300), recording("b", "b"))]),
            work("c", vec![lasting(Some(500), recording("c", "c"))]),
            work("d", vec![lasting(None, recording("d", "d"))]),
        ];
        let median = median_length(&works);
        assert_eq!(median, Some(300));
        let unknown = &works[3].recordings[0];
        assert!((counted_length(unknown, median) - 300.0).abs() < f64::EPSILON);

        // With no length known at all, they count the same
        let works = [
            work("a", vec![lasting(None, recording("a", "a"))]),
            work("b", vec![lasting(None, recording("b", "b"))]),
        ];
        let median = median_length(&works);
        assert_eq!(median, None);
        let counted = counted_length(&works[0].recordings[0], median);
        assert!((counted - 1.0).abs() < f64::EPSILON);
        let picks = picks(&works, 6);
        let a = f64::from(picks[&(0, 0)]) / 2000.0;
        assert!((0.45..0.55).contains(&a), "{a}");
    }

    #[test]
    fn the_toast_says_what_of_a_work_plays() {
        use crate::audio::Arrangement;

        let whole = recording("a", "w");
        assert_eq!(Part::of(&whole), None);
        let aria = RecordingCandidate {
            single_movement: true,
            ..recording("b", "w")
        };
        assert_eq!(Part::of(&aria), Some(Part::Movement));
        let arrangement = Arrangement {
            of: "The work".to_string(),
            of_id: Some("w".to_string()),
            label: Some("for piano, Busoni".to_string()),
        };
        let busoni = RecordingCandidate {
            arrangement: Some(arrangement.clone()),
            ..recording("c", "busoni")
        };
        assert_eq!(
            Part::of(&busoni),
            Some(Part::Arrangement("for piano, Busoni".to_string()))
        );
        // A single movement of an arrangement is still only part of the
        // work
        let movement = RecordingCandidate {
            single_movement: true,
            arrangement: Some(arrangement),
            ..recording("d", "busoni")
        };
        assert_eq!(Part::of(&movement), Some(Part::Movement));
    }
}
