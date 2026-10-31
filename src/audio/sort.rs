// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The order of a sort of the tracks of a playlist, by album, artist,
// composer, title, year or file name, or the other way round. The movements
// of a work that are next to each other move together, in their order, as
// in a shuffle, and go by the first of them. Tracks that go in the same
// place are put in the order of their albums, and those still the same
// keep the order they're in

use std::path::{Path, PathBuf};

use gtk::glib;

use crate::{
    cue_sheet,
    library::{self, TrackOrder, WorkComposer, WorkToOrder},
    utils,
};

use super::shuffle::work_units;

// What the tracks of a playlist can be sorted by, as the targets of the
// action that sorts them
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Album,
    Artist,
    Composer,
    Title,
    Year,
    FileName,
    Reverse,
}

impl SortKey {
    pub fn from_nick(nick: &str) -> Option<Self> {
        match nick {
            "album" => Some(Self::Album),
            "artist" => Some(Self::Artist),
            "composer" => Some(Self::Composer),
            "title" => Some(Self::Title),
            "year" => Some(Self::Year),
            "file-name" => Some(Self::FileName),
            "reverse" => Some(Self::Reverse),
            _ => None,
        }
    }
}

// What a sort knows of a track
#[derive(Debug, Default)]
pub(super) struct Entry {
    // Whether the track is a movement that continues the work of the
    // track before it, as in the headers of the queue (`continues_work()`)
    pub continues_work: bool,

    // The album, as the tags have it and as it's shown, which is the name
    // of its folder for a track without one
    pub album: Option<String>,
    pub shown_album: String,
    pub album_artist: Option<String>,
    // The MusicBrainz id of the release
    pub release: Option<String>,
    // The folder of the album, where disc folders count as their parent,
    // see `library::album_folder()`
    pub folder: Option<PathBuf>,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    // Where the track is: a path under its sheet for a track of a CUE sheet
    pub path: PathBuf,

    pub artist: Option<String>,
    pub artist_sort: Option<String>,

    // The composers of the work, or of the track, see
    // `library::credited_work_composers()`
    pub composers: Vec<WorkComposer>,
    // What tells the work apart, see `library::WorkToOrder`
    pub work_key: String,
    // The name of the work, or else the title, and the work's full title
    pub work_name: String,
    pub work_title: Option<String>,

    // The work of a movement, or else the title, as the queue shows it
    pub heading: String,
    // See `TrackCredits::year()`
    pub year: Option<String>,
}

// The positions of the tracks in their sorted order
pub(super) fn sorted_order(entries: &[Entry], key: SortKey) -> Vec<usize> {
    let units = work_units(entries.iter().map(|entry| entry.continues_work));
    let firsts: Vec<&Entry> = units.iter().map(|unit| &entries[unit.start]).collect();

    let order = match key {
        SortKey::Reverse => (0..units.len()).rev().collect(),
        SortKey::Album => by_keys(&firsts, vec![(); firsts.len()]),
        SortKey::Artist => {
            let keys = firsts
                .iter()
                .map(|entry| {
                    let artist = present(entry.artist_sort.as_deref())
                        .or_else(|| present(entry.artist.as_deref()))
                        .map(glib::CollationKey::from);
                    (last(artist), last(entry.year.as_deref()))
                })
                .collect();
            by_keys(&firsts, keys)
        }
        SortKey::Composer => {
            // Tracks without composers are missing them, as tracks are
            // missing a value in the other sorts, and go last in the order
            // of their albums, rather than by their names
            let works: Vec<WorkToOrder> = firsts
                .iter()
                .map(|entry| {
                    let named = !entry.composers.is_empty();
                    WorkToOrder {
                        key: &entry.work_key,
                        composers: &entry.composers,
                        name: if named { &entry.work_name } else { "" },
                        title: entry.work_title.as_deref().filter(|_| named),
                    }
                })
                .collect();
            by_keys(&firsts, library::work_orders(&works))
        }
        SortKey::Title => {
            let keys = firsts
                .iter()
                .map(|entry| glib::FilenameCollationKey::from(entry.heading.as_str()))
                .collect();
            by_keys(&firsts, keys)
        }
        SortKey::Year => {
            let keys = firsts
                .iter()
                .map(|entry| last(entry.year.as_deref()))
                .collect();
            by_keys(&firsts, keys)
        }
        SortKey::FileName => {
            let keys = firsts
                .iter()
                .map(|entry| file_name_order(&entry.path))
                .collect();
            by_keys(&firsts, keys)
        }
    };
    order
        .into_iter()
        .flat_map(|unit| units[unit].clone())
        .collect()
}

// The units in the order of their keys, then of their albums; a stable
// sort, so the units that tie keep their order
fn by_keys<K: Ord>(firsts: &[&Entry], keys: Vec<K>) -> Vec<usize> {
    let albums: Vec<AlbumOrder> = firsts.iter().map(|entry| album_order(entry)).collect();
    let mut order: Vec<usize> = (0..firsts.len()).collect();
    order.sort_by(|&a, &b| {
        keys[a]
            .cmp(&keys[b])
            .then_with(|| albums[a].cmp(&albums[b]))
    });
    order
}

// Where a track goes by its album: by the album's title, a missing one
// last, then by the album artist, then by what tells apart albums with the
// same title and album artist, and then in the order of the album
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct AlbumOrder {
    untagged: bool,
    title: glib::FilenameCollationKey,
    album_artist: (bool, Option<glib::CollationKey>),
    identity: (Option<String>, Option<glib::FilenameCollationKey>),
    track: TrackOrder,
}

fn album_order(entry: &Entry) -> AlbumOrder {
    let album = present(entry.album.as_deref());
    let album_artist = present(entry.album_artist.as_deref());
    // As the library tells albums apart (`album_id`): by their release, if
    // they have a title, or else, when they have a title and an album
    // artist, by those, so that the discs of an album in folders of their
    // own stay together, or else by their folder, as compilations often
    // have no album artist, and tracks without an album are grouped by the
    // folder they're in
    let release = album.and(present(entry.release.as_deref()));
    let folder = (release.is_none() && (album.is_none() || album_artist.is_none()))
        .then_some(entry.folder.as_deref())
        .flatten()
        .map(|folder| glib::FilenameCollationKey::from(folder.to_string_lossy().as_ref()));
    AlbumOrder {
        untagged: album.is_none(),
        title: glib::FilenameCollationKey::from(entry.shown_album.as_str()),
        album_artist: last(album_artist.map(glib::CollationKey::from)),
        identity: (release.map(ToString::to_string), folder),
        track: TrackOrder::new(
            &entry.path,
            entry.disc.map(i64::from),
            entry.track.map(i64::from),
        ),
    }
}

// Where a track goes by its file name: the name of its sheet, then its
// number in it, for a track of a CUE sheet, so that the tracks of a sheet
// stay together and in order. Only the path is looked at, not the disk
fn file_name_order(path: &Path) -> ((bool, glib::FilenameCollationKey), Option<u32>) {
    let (file, number) = match cue_sheet::missing_location_parts(path) {
        Some((sheet, number)) => (sheet, Some(number)),
        None => (path, None),
    };
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    (utils::file_name_key(&name), number)
}

// A value, with the missing ones last
fn last<T: Ord>(value: Option<T>) -> (bool, Option<T>) {
    (value.is_none(), value)
}

// A value from the tags, unless it's empty
fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_permutation(order: &[usize], n: usize) -> bool {
        let mut sorted = order.to_vec();
        sorted.sort_unstable();
        sorted == (0..n).collect::<Vec<_>>()
    }

    // A track of an album, its number on it, in the album's folder
    fn track(album: &str, number: u32) -> Entry {
        Entry {
            album: Some(album.to_string()),
            shown_album: album.to_string(),
            album_artist: Some("Artist".to_string()),
            folder: Some(PathBuf::from(format!("/music/{album}"))),
            track: Some(number),
            path: PathBuf::from(format!("/music/{album}/{number:02}.flac")),
            heading: format!("Song {number}"),
            work_name: format!("Song {number}"),
            work_key: format!("song {number}"),
            ..Entry::default()
        }
    }

    fn with(entry: Entry, change: impl FnOnce(&mut Entry)) -> Entry {
        let mut entry = entry;
        change(&mut entry);
        entry
    }

    fn composer(name: &str, sort: &str) -> WorkComposer {
        WorkComposer {
            key: name.to_lowercase(),
            name: name.to_string(),
            sort: sort.to_string(),
        }
    }

    // A movement of a work, each after the first continuing it
    fn movement(work: &str, album: &str, number: u32, first: bool) -> Entry {
        with(track(album, number), |entry| {
            entry.continues_work = !first;
            entry.heading = work.to_string();
            entry.work_name = work.to_string();
            entry.work_key = work.to_lowercase();
            entry.composers = vec![composer("Johann Sebastian Bach", "Bach, Johann Sebastian")];
        })
    }

    const KEYS: [SortKey; 7] = [
        SortKey::Album,
        SortKey::Artist,
        SortKey::Composer,
        SortKey::Title,
        SortKey::Year,
        SortKey::FileName,
        SortKey::Reverse,
    ];

    #[test]
    fn the_names_of_the_sorts_are_those_of_the_action() {
        assert_eq!(SortKey::from_nick("file-name"), Some(SortKey::FileName));
        assert_eq!(SortKey::from_nick("reverse"), Some(SortKey::Reverse));
        assert_eq!(SortKey::from_nick("added"), None);
    }

    #[test]
    fn each_sort_has_every_track_once() {
        let entries = vec![
            track("B", 2),
            movement("Partita", "A", 1, true),
            movement("Partita", "A", 2, false),
            Entry::default(),
            track("C", 1),
            track("B", 1),
        ];
        for key in KEYS {
            let order = sorted_order(&entries, key);
            assert!(is_permutation(&order, entries.len()), "{key:?}: {order:?}");
        }
    }

    #[test]
    fn albums_go_by_title_then_in_the_order_of_the_album() {
        let entries = [track("B", 2), track("A", 2), track("B", 1), track("A", 1)];
        assert_eq!(sorted_order(&entries, SortKey::Album), [3, 1, 2, 0]);
    }

    #[test]
    fn ties_go_by_album_and_tracks_that_tie_keep_their_order() {
        // The same year, on two albums, and two copies of a track
        let year = |entry: Entry| with(entry, |e| e.year = Some("1990".to_string()));
        let entries = [
            year(track("B", 1)),
            year(track("A", 1)),
            year(track("A", 1)),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Year), [1, 2, 0]);
    }

    #[test]
    fn missing_values_go_last() {
        let entries = [
            with(track("A", 1), |e| e.year = None),
            with(track("B", 1), |e| e.year = Some("2001".to_string())),
            with(track("C", 1), |e| e.year = Some("1971".to_string())),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Year), [2, 1, 0]);

        let untagged = with(track("A", 1), |e| e.album = None);
        let entries = [untagged, track("B", 1)];
        assert_eq!(sorted_order(&entries, SortKey::Album), [1, 0]);

        let entries = [
            with(track("A", 1), |e| e.artist = None),
            with(track("B", 1), |e| e.artist = Some("Zappa".to_string())),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Artist), [1, 0]);
    }

    #[test]
    fn albums_the_library_tells_apart_stay_apart() {
        let in_folder = |entry: Entry, folder: &str| {
            with(entry, |e| {
                e.folder = Some(PathBuf::from(folder));
                e.path = PathBuf::from(format!("{folder}/{:02}.flac", e.track.unwrap()));
            })
        };

        // Compilations with the same title and no album artist
        let compilation = |folder, number| {
            with(in_folder(track("Greatest Hits", number), folder), |e| {
                e.album_artist = None;
            })
        };
        let entries = [
            compilation("/music/x", 1),
            compilation("/music/y", 1),
            compilation("/music/x", 2),
            compilation("/music/y", 2),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Album), [0, 2, 1, 3]);

        // Untagged folders with the same name, by the same album artist
        let live =
            |folder, number| with(in_folder(track("Live", number), folder), |e| e.album = None);
        let entries = [
            live("/music/1999/Live", 1),
            live("/music/2004/Live", 1),
            live("/music/1999/Live", 2),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Album), [0, 2, 1]);

        // Releases with the same title and album artist
        let release = |id: &str, number| {
            with(track("Symphonies", number), |e| {
                e.release = Some(id.to_string());
            })
        };
        let entries = [release("b", 1), release("a", 1), release("b", 2)];
        assert_eq!(sorted_order(&entries, SortKey::Album), [1, 0, 2]);
    }

    #[test]
    fn the_discs_of_an_album_in_folders_of_their_own_stay_together() {
        // Folders that aren't disc folders, as "Disc 1" would be, with the
        // album's title and album artist
        let disc = |folder: &str, disc, number| {
            with(track("Ring", number), |e| {
                e.folder = Some(PathBuf::from(folder));
                e.disc = Some(disc);
                e.path = PathBuf::from(format!("{folder}/{number:02}.flac"));
            })
        };
        let entries = [
            disc("/music/Ring (CD 2)", 2, 1),
            disc("/music/Ring (CD 1)", 1, 2),
            disc("/music/Ring (CD 1)", 1, 1),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Album), [2, 1, 0]);
    }

    #[test]
    fn artists_go_by_their_sort_names() {
        let artist = |name: &str, sort: Option<&str>, album| {
            with(track(album, 1), |e| {
                e.artist = Some(name.to_string());
                e.artist_sort = sort.map(ToString::to_string);
            })
        };
        let entries = [
            artist("The Beatles", Some("Beatles, The"), "A"),
            artist("Cream", None, "B"),
            // Not turned around without a sort name
            artist("Pink Floyd", None, "C"),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Artist), [0, 1, 2]);
    }

    #[test]
    fn composers_go_by_their_sort_names_then_by_catalogue_number() {
        let work = |name: &str, sort: &str, title: &str, album| {
            with(track(album, 1), |e| {
                e.composers = vec![composer(name, sort)];
                e.work_name = title.to_string();
                e.work_key = title.to_lowercase();
            })
        };
        let entries = [
            work(
                "Johann Sebastian Bach",
                "Bach, Johann Sebastian",
                "Suite",
                "A",
            ),
            work(
                "Johann Sebastian Bach",
                "Bach, Johann Sebastian",
                "Cello Suite No. 1, BWV 1007",
                "B",
            ),
            work("Antonin Dvorak", "Dvorak, Antonin", "Cello Concerto", "C"),
            work(
                "Johann Sebastian Bach",
                "Bach, Johann Sebastian",
                "Partita No. 2, BWV 1004",
                "D",
            ),
            // No composers, last
            with(track("E", 1), |e| e.work_name = "Anything".to_string()),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Composer), [3, 1, 0, 2, 4]);
    }

    #[test]
    fn tracks_without_composers_go_last_in_the_order_of_their_albums() {
        let song = |title: &str, album| with(track(album, 1), |e| e.work_name = title.to_string());
        let entries = [
            song("Help", "B"),
            song("Crossroads", "C"),
            song("Yesterday", "A"),
            with(track("D", 1), |e| {
                e.composers = vec![composer("Johann Sebastian Bach", "Bach, Johann Sebastian")];
            }),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Composer), [3, 2, 0, 1]);
    }

    #[test]
    fn a_composers_main_catalogue_counts_each_work_once() {
        let work = |title: &str, album| {
            with(track(album, 1), |e| {
                e.composers = vec![composer("Ludwig van Beethoven", "Beethoven, Ludwig van")];
                e.work_name = title.to_string();
                e.work_key = title.to_lowercase();
            })
        };
        // Three recordings of a WoO work, and two works with opus numbers:
        // the opus numbers are the main catalogue, which comes first
        let entries = [
            work("Fur Elise, WoO 59", "A"),
            work("Fur Elise, WoO 59", "B"),
            work("Fur Elise, WoO 59", "C"),
            work("Sonata, Op. 27 No. 2", "D"),
            work("Sonata, Op. 13", "E"),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Composer), [4, 3, 0, 1, 2]);
    }

    #[test]
    fn titles_go_in_natural_order_and_works_by_their_name() {
        let titled = |title: &str, album| with(track(album, 1), |e| e.heading = title.to_string());
        // With the same capitals, as the tests run in the C locale
        let entries = [
            titled("No. 10", "A"),
            movement("Partita", "B", 1, true),
            movement("Partita", "B", 2, false),
            titled("No. 2", "C"),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Title), [3, 0, 1, 2]);
    }

    #[test]
    fn the_tracks_of_a_cue_sheet_go_by_the_sheet_then_their_number() {
        let cue = |sheet: &str, number: u32| {
            with(track("A", number), |e| {
                e.path = PathBuf::from(format!("/music/{sheet}.cue/track{number:02}"));
            })
        };
        let file = |name: &str| with(track("B", 1), |e| e.path = PathBuf::from(name));
        let entries = [
            cue("Zeta", 2),
            file("/music/b/Alpha.flac"),
            cue("Zeta", 1),
            file("/music/b/Omega.flac"),
        ];
        assert_eq!(sorted_order(&entries, SortKey::FileName), [1, 3, 2, 0]);
    }

    #[test]
    fn the_movements_of_a_work_stay_together_in_order_whatever_the_sort() {
        let entries = [
            track("Z", 1),
            movement("Partita", "M", 1, true),
            movement("Partita", "M", 2, false),
            movement("Partita", "M", 3, false),
            track("A", 1),
        ];
        for key in KEYS {
            let order = sorted_order(&entries, key);
            let first = order.iter().position(|&t| t == 1).unwrap();
            assert_eq!(order[first..first + 3], [1, 2, 3], "{key:?}: {order:?}");
        }
    }

    #[test]
    fn reversing_twice_gives_the_order_back() {
        // Tracks of different albums, and a work that no other unit
        // continues, so the units are the same either way round
        let entries = [
            track("A", 1),
            movement("Partita", "B", 1, true),
            movement("Partita", "B", 2, false),
            track("C", 1),
        ];
        let reversed = sorted_order(&entries, SortKey::Reverse);
        assert_eq!(reversed, [3, 1, 2, 0]);
        let again: Vec<Entry> = reversed
            .iter()
            .map(|&t| Entry {
                continues_work: entries[t].continues_work,
                ..track("x", 1)
            })
            .collect();
        let twice: Vec<usize> = sorted_order(&again, SortKey::Reverse)
            .into_iter()
            .map(|t| reversed[t])
            .collect();
        assert_eq!(twice, [0, 1, 2, 3]);
    }

    #[test]
    fn reversing_can_join_the_movements_of_a_work() {
        // The third movement before the first two: two units, which come
        // out in order, as one unit, which reversing again leaves alone
        let entries = [
            movement("Partita", "B", 3, true),
            movement("Partita", "B", 1, true),
            movement("Partita", "B", 2, false),
        ];
        assert_eq!(sorted_order(&entries, SortKey::Reverse), [1, 2, 0]);
        let joined = [
            movement("Partita", "B", 1, true),
            movement("Partita", "B", 2, false),
            movement("Partita", "B", 3, false),
        ];
        assert_eq!(sorted_order(&joined, SortKey::Reverse), [0, 1, 2]);
    }
}
