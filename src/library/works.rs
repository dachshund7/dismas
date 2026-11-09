// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// A work of the albums of the grid, see `WorkEntry`
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkInfo {
    pub(crate) key: String,
    // The name its tracks give it the most, and its full title, if that's
    // not the name
    pub(crate) name: String,
    pub(crate) title: Option<String>,
    // The work it's part of, or the collection it's in
    pub(crate) collection: Option<String>,
    // For an arrangement, the work it arranges, and what it is, if
    // MusicBrainz says
    pub(crate) arrangement: Option<Arrangement>,
    // The keys of the arrangements of it that are listed under it, rather
    // than on their own, see `listed_under()`
    pub(crate) arrangements: Vec<String>,
    pub(crate) composers: Vec<WorkComposer>,
    // The number of its recordings that the filter matches, those of its
    // arrangements included, see `recordings_of_works()`
    pub(crate) recordings: u32,
    // Whether it's a favorite, or one of its recordings is; they're in the
    // play stats, as they can't be read again from the music
    pub(crate) favorite: bool,
}

impl WorkInfo {
    // Its key and those of the arrangements listed under it, the key first,
    // which take its tracks from the albums, see `TrackFilter`
    pub(crate) fn keys(&self) -> Vec<String> {
        self.all_keys().cloned().collect()
    }

    fn all_keys(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.key).chain(&self.arrangements)
    }

    // Whether it's among the `favorites`, the keys of the works marked and
    // of those of the favorite recordings, see
    // `play_stats::favorite_work_keys()`: by its own key, or by that of an
    // arrangement listed under it
    pub(crate) fn is_among(&self, favorites: &HashSet<String>) -> bool {
        self.all_keys().any(|key| favorites.contains(key))
    }

    // The names of its composers, as a list
    pub(crate) fn composer_names(&self) -> String {
        self.composers
            .iter()
            .map(|composer| &composer.name)
            .join(", ")
    }

    // The keys of its composers, which tell the works of other composers
    // apart, see `composer_key()`
    pub(crate) fn composer_keys(&self) -> impl Iterator<Item = &str> {
        self.composers.iter().map(|composer| composer.key.as_str())
    }
}

// A composer of a work, see `composer_entries()`
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkComposer {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) sort: String,
}

// A recording of a work: its tracks on an album, see
// `recordings_of_works()`
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkRecording {
    pub(crate) album_id: String,
    pub(crate) year: Option<String>,
    // The conductors and performers of its tracks, as the credits line of
    // a track names them, see `performers_line()`
    pub(crate) performers: String,
    pub(crate) paths: Vec<PathBuf>,
    // The key of the work of its first track, which it's found by, e.g. as
    // a favorite
    pub(crate) work_key: String,
    // What it is, if it's all of an arrangement listed under the work, see
    // `listed_under()`
    pub(crate) arrangement: Option<Arrangement>,
}

// A track of an album with works, as the recordings of the works are
// worked out from
pub(super) struct WorkRow {
    pub(super) album_id: String,
    pub(super) path: PathBuf,
    work: String,
    // See `WorkEntry::order`, with "" as an empty list
    order: Option<Vec<u32>>,
    // Whether the filter matches it
    matching: bool,
    name: Option<String>,
    title: Option<String>,
    collection: Option<String>,
    composers: Option<String>,
    pub(super) album: Option<String>,
    // When it was recorded, see `SortDates::recorded`
    recorded: Option<TagDate>,
    performers: Option<String>,
    arrangement_of: Option<String>,
    arrangement: Option<String>,
    arrangement_of_id: Option<String>,
    // See `WorkEntry::part_ids`
    part_ids: Vec<String>,
    // In whole seconds, if it's known and was asked for, see
    // `query_work_rows()`
    length: Option<u64>,
}

impl WorkRow {
    // The work its work arranges, if it's an arrangement
    fn arrangement(&self) -> Option<Arrangement> {
        Some(Arrangement {
            of: self.arrangement_of.clone()?,
            of_id: self.arrangement_of_id.clone(),
            label: self.arrangement.clone().filter(|label| !label.is_empty()),
        })
    }
}

// Whether a track of the albums of the grid is part of a work, without the
// tracks in the `hidden` folders
pub(crate) fn has_works(hidden: &[PathBuf]) -> bool {
    query_or_default("look for works", |conn| {
        query_tracks_have(conn, "work_key", hidden)
    })
}

// The works of the albums of the grid with a recording that the filter
// matches, by their composers' sort names and then their names, with the
// numbers in them in order, e.g. "no. 2" before "no. 10"; the works
// without a composer are last. An arrangement of one of them is listed
// under it rather than on its own, see `listed_under()`. The tracks in the
// `hidden` folders are left out
pub(crate) fn works(filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<WorkInfo> {
    query_or_default("list the works", |conn| query_works(conn, filter, hidden))
}

pub(super) fn query_works(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkInfo>> {
    let rows = query_work_rows(conn, filter, &[], hidden, Lengths::Skip)?;
    Ok(works_of_rows(&rows, &mut RecordingDates::new(conn)))
}

// A work the Works page lists, as Play a Random Work picks from them
#[derive(Debug)]
pub(crate) struct WorkCandidate {
    pub(crate) info: WorkInfo,
    // The recordings of it that the filter matches, its own and those of
    // the arrangements listed under it
    pub(crate) recordings: Vec<RecordingCandidate>,
    // The tracks of all its recordings, those the filter doesn't match
    // too, which tell when it was heard last
    pub(crate) paths: Vec<PathBuf>,
}

// A recording of a work that Play a Random Work can play
#[derive(Debug)]
pub(crate) struct RecordingCandidate {
    pub(crate) album_id: String,
    // The work of its first track, by which a favorite recording is kept,
    // see `play_stats::FavoriteRecording`
    pub(crate) work_key: String,
    // Its tracks, in the order of their album
    pub(crate) paths: Vec<PathBuf>,
    // Its length in whole seconds, from those of its tracks that are known,
    // as if the others were as long, or `None` when none is
    pub(crate) length: Option<u64>,
    // Whether it's a single movement of the work, in one track or several,
    // such as an aria of an opera on a recital
    pub(crate) single_movement: bool,
    // What it is, if it's all of an arrangement listed under the work, as
    // in `WorkRecording`
    pub(crate) arrangement: Option<Arrangement>,
}

// The works that `works()` lists for the filter, in its order, each with
// the recordings of it that the filter matches, as `work_recordings()`
// gives them with its keys; the tracks in the `hidden` folders are left
// out
pub(crate) fn work_recording_candidates(
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> Vec<WorkCandidate> {
    query_or_default("load the recordings of the works", |conn| {
        query_work_recording_candidates(conn, filter, hidden)
    })
}

pub(super) fn query_work_recording_candidates(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkCandidate>> {
    let rows = query_work_rows(conn, filter, &[], hidden, Lengths::Read)?;
    Ok(listed_works(&rows, &mut RecordingDates::new(conn))
        .into_iter()
        .map(|listed| {
            let arranged = &listed.info.arrangements;
            WorkCandidate {
                recordings: listed
                    .recordings
                    .iter()
                    .filter(|recording| recording.matching)
                    .map(|recording| recording_candidate(&rows, &recording.rows, arranged))
                    .collect(),
                paths: listed
                    .recordings
                    .iter()
                    .flat_map(|recording| &recording.rows)
                    .map(|&i| rows[i].path.clone())
                    .collect(),
                info: listed.info,
            }
        })
        .collect())
}

// A recording of a work, from its `rows`, with the keys of the
// arrangements `arranged` under the work
fn recording_candidate(
    rows: &[WorkRow],
    recording: &[usize],
    arranged: &[String],
) -> RecordingCandidate {
    let rows: Vec<&WorkRow> = recording.iter().map(|&i| &rows[i]).collect();
    let first = rows[0];
    let known: Vec<u64> = rows.iter().filter_map(|row| row.length).collect();
    let tracks = rows.len() as u64;
    let place = first.order.as_ref().filter(|place| !place.is_empty());
    RecordingCandidate {
        album_id: first.album_id.clone(),
        work_key: first.work.clone(),
        paths: rows.iter().map(|row| row.path.clone()).collect(),
        length: (!known.is_empty())
            .then(|| known.iter().sum::<u64>() * tracks / known.len() as u64),
        single_movement: place
            .is_some_and(|place| rows.iter().all(|row| row.order.as_ref() == Some(place))),
        arrangement: recording_arrangement(&rows, |work| arranged.iter().any(|key| key == work)),
    }
}

// What a recording is, from its rows, when they're all of arrangements
// listed under its work, as `arranged` tells by their keys, rather than
// of the work itself
fn recording_arrangement(
    rows: &[&WorkRow],
    arranged: impl Fn(&str) -> bool,
) -> Option<Arrangement> {
    rows.iter()
        .all(|row| arranged(&row.work))
        .then(|| rows.first()?.arrangement())
        .flatten()
}

// The works of the rows with a recording that the filter matches, sorted
// as in `works()`, see `listed_works()`
fn works_of_rows(rows: &[WorkRow], dates: &mut RecordingDates) -> Vec<WorkInfo> {
    listed_works(rows, dates)
        .into_iter()
        .map(|listed| listed.info)
        .collect()
}

// The rows of a recording of a work, see `recordings_of_works()`, and
// whether the filter matches any of them
pub(super) struct RecordingRows {
    pub(super) rows: Vec<usize>,
    matching: bool,
}

// A work as the Works page lists it, with the recordings of it and of the
// arrangements listed under it, those the filter matches or not
pub(super) struct ListedWork {
    pub(super) info: WorkInfo,
    pub(super) recordings: Vec<RecordingRows>,
}

// The works of the rows with a recording that the filter matches, sorted
// as in `works()`. The arrangements of a work among them are listed under
// it, see `listed_under()`, and their recordings count as its own. The
// `dates` are read as `recordings_of_works()` needs them
pub(super) fn listed_works(rows: &[WorkRow], dates: &mut RecordingDates) -> Vec<ListedWork> {
    // How often each of the names of a work is given, by all its tracks
    #[derive(Default)]
    struct Seen<'a> {
        names: HashMap<&'a str, usize>,
        titles: HashMap<&'a str, usize>,
        collections: HashMap<&'a str, usize>,
        composers: HashMap<&'a str, usize>,
        arrangements_of: HashMap<&'a str, usize>,
        arrangements: HashMap<&'a str, usize>,
        arrangement_of_ids: HashMap<&'a str, usize>,
    }
    fn count_some<'a>(counts: &mut HashMap<&'a str, usize>, value: Option<&'a str>) {
        if let Some(value) = value {
            *counts.entry(value).or_default() += 1;
        }
    }

    let mut seen: HashMap<&str, Seen> = HashMap::new();
    for row in rows {
        let work = seen.entry(&row.work).or_default();
        count_some(&mut work.names, row.name.as_deref());
        count_some(&mut work.titles, row.title.as_deref());
        count_some(&mut work.collections, row.collection.as_deref());
        count_some(&mut work.composers, row.composers.as_deref());
        count_some(&mut work.arrangements_of, row.arrangement_of.as_deref());
        count_some(&mut work.arrangements, row.arrangement.as_deref());
        count_some(
            &mut work.arrangement_of_ids,
            row.arrangement_of_id.as_deref(),
        );
    }

    let most = |counts: HashMap<&str, usize>| Some(most_common(counts)).filter(|s| !s.is_empty());
    let mut works: HashMap<&str, WorkInfo> = seen
        .into_iter()
        .map(|(key, seen)| {
            let composers = most(seen.composers)
                .map(|entries| work_composers(&entries))
                .unwrap_or_default();
            let work = WorkInfo {
                key: key.to_owned(),
                name: most(seen.names).unwrap_or_default(),
                title: most(seen.titles),
                collection: most(seen.collections),
                arrangement: most(seen.arrangements_of).map(|of| Arrangement {
                    of,
                    of_id: most(seen.arrangement_of_ids),
                    label: most(seen.arrangements),
                }),
                arrangements: Vec::new(),
                composers,
                recordings: 0,
                favorite: false,
            };
            (key, work)
        })
        .collect();

    let listed = listed_under(rows, &works);
    // The recordings of each work, by the work they're listed under
    let mut recordings: HashMap<&str, Vec<RecordingRows>> = HashMap::new();
    for recording in recordings_of_works(rows, |row| split_recording_work(row, &listed), dates) {
        let first = rows[recording[0]].work.as_str();
        let key = listed.get(first).copied().unwrap_or(first);
        let matching = recording.iter().any(|&i| rows[i].matching);
        if matching && let Some(work) = works.get_mut(key) {
            work.recordings += 1;
        }
        recordings.entry(key).or_default().push(RecordingRows {
            rows: recording,
            matching,
        });
    }
    let mut arranged: Vec<(&str, &str)> = listed.into_iter().collect();
    arranged.sort_unstable();
    for (key, under) in arranged {
        if works
            .remove(key)
            .is_some_and(|work| work.arrangement.is_some())
            && let Some(work) = works.get_mut(under)
        {
            work.arrangements.push(key.to_owned());
        }
    }
    sort_works(
        works
            .into_values()
            .filter(|work| work.recordings > 0)
            .collect(),
    )
    .into_iter()
    .map(|info| ListedWork {
        recordings: recordings.remove(info.key.as_str()).unwrap_or_default(),
        info,
    })
    .collect()
}

// The arrangements among the works, by key, each with the work it's listed
// under on the Works page rather than on its own: the work it arranges,
// found by its MusicBrainz id, or, for an arrangement whose tracks are
// movements, as the tags make an arrangement of a single movement, the
// work with that movement among its parts, the first by key if there are
// several. Only when the two share a composer, as an arrangement that
// MusicBrainz credits to the arranger alone is a work of theirs, and a
// whole work that arranges a single movement, e.g. Barber's Adagio for
// Strings, stays on its own too. An arrangement of an arrangement goes
// under the work that's listed on its own
fn listed_under<'a>(
    rows: &'a [WorkRow],
    works: &HashMap<&'a str, WorkInfo>,
) -> HashMap<&'a str, &'a str> {
    let mut with_part: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    let mut of_movements: HashSet<&str> = HashSet::new();
    for row in rows {
        for id in &row.part_ids {
            with_part.entry(id).or_default().insert(&row.work);
        }
        if row.order.is_some() {
            of_movements.insert(&row.work);
        }
    }
    let share_a_composer = |a: &WorkInfo, b: &WorkInfo| {
        a.composer_keys()
            .any(|key| b.composer_keys().contains(&key))
    };

    let mut parents: HashMap<&str, &str> = HashMap::new();
    for (&key, work) in works {
        let Some(of_id) = work.arrangement.as_ref().and_then(|a| a.of_id.as_deref()) else {
            continue;
        };
        let arranged = works.get_key_value(of_id).map(|(&key, _)| key);
        let with_movement = with_part
            .get(of_id)
            .filter(|_| of_movements.contains(key))
            .into_iter()
            .flatten()
            .copied();
        if let Some(parent) = arranged
            .into_iter()
            .chain(with_movement)
            .find(|&parent| parent != key && share_a_composer(work, &works[parent]))
        {
            parents.insert(key, parent);
        }
    }
    // Up to the work that's on its own, leaving out a loop
    parents
        .keys()
        .filter_map(|&key| {
            let mut passed = HashSet::from([key]);
            let mut under = parents[key];
            while let Some(&next) = parents.get(under) {
                if !passed.insert(under) {
                    return None;
                }
                under = next;
            }
            Some((key, under))
        })
        .collect()
}

// The work a track's recording is one of, with the arrangements `listed`
// under another work: a movement of an arrangement goes with those of
// the work, as MusicBrainz sometimes links some of the movements of a
// recording to an arrangement of the work, and a recording split between
// the two is one recording. The tracks of whole works keep their own, as
// an arrangement of a movement next to the work on an album is a
// recording of its own, e.g. its Chaconne on the lute after a partita
fn split_recording_work<'a>(row: &'a WorkRow, listed: &HashMap<&'a str, &'a str>) -> &'a str {
    match listed.get(row.work.as_str()) {
        Some(under) if row.order.is_some() => under,
        _ => &row.work,
    }
}

// Where a work goes among the works of its composers, by its catalogue
// number: first the works in the catalogue that the most of them are in,
// by their numbers, then the works in another catalogue, by it, and then
// those without a number
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum CatalogueOrder {
    Main(Vec<(u32, String)>),
    Other(CatalogueNumber),
    None,
}

// The works under their composers, sorted by the composers' sort names,
// and the works of each by their catalogue numbers, then by name, "no. 2"
// before "no. 10"
fn sort_works(works: Vec<WorkInfo>) -> Vec<WorkInfo> {
    let orders = {
        let to_order: Vec<WorkToOrder> = works
            .iter()
            .map(|work| WorkToOrder {
                key: &work.key,
                composers: &work.composers,
                name: &work.name,
                title: work.title.as_deref(),
            })
            .collect();
        work_orders(&to_order)
    };
    let mut sorted: Vec<(WorkOrder, WorkInfo)> = orders.into_iter().zip(works).collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.key.cmp(&b.1.key)));
    sorted.into_iter().map(|(_, work)| work).collect()
}

// A work to put in order among others, see `work_orders()`
pub(crate) struct WorkToOrder<'a> {
    // What tells the work apart, so that it counts once for the catalogue
    // of its composers however many times it's there
    pub(crate) key: &'a str,
    pub(crate) composers: &'a [WorkComposer],
    pub(crate) name: &'a str,
    // Its full title, whose catalogue numbers count when its name has none
    pub(crate) title: Option<&'a str>,
}

// Where a work goes among others, see `work_orders()`
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WorkOrder {
    no_composers: bool,
    first_composer: Option<glib::CollationKey>,
    composers: String,
    catalogue: CatalogueOrder,
    name: glib::FilenameCollationKey,
}

// Where each work goes among the others, as the Works page lists them: by
// the sort name of their first composer, then their composers, those
// without any last, and the works of each composer by their catalogue
// numbers, then by name. The playlist's Composer sort goes by it too, so
// a composer's works are in the same order in both
pub(crate) fn work_orders(works: &[WorkToOrder]) -> Vec<WorkOrder> {
    // The numbers in the name of each work, or else in its full title
    let numbers: Vec<Vec<CatalogueNumber>> = works
        .iter()
        .map(|work| {
            let numbers = catalogue::catalogue_numbers(work.name);
            match work.title {
                Some(title) if numbers.is_empty() => catalogue::catalogue_numbers(title),
                _ => numbers,
            }
        })
        .collect();
    let composers: Vec<String> = works
        .iter()
        .map(|work| work.composers.iter().map(|c| c.key.as_str()).join("\x1f"))
        .collect();

    // The catalogue of each composer's works, the one the most of them are
    // in, as Dvořák's opus numbers or Beethoven's; an even count goes to
    // the first in the list of catalogues, which is the same every time.
    // Each work counts once, as a playlist can have several recordings
    // of one
    let mut counted: HashSet<(&str, &str)> = HashSet::new();
    let mut counts: HashMap<&str, HashMap<&'static str, usize>> = HashMap::new();
    for ((work, composer), numbers) in works.iter().zip(&composers).zip(&numbers) {
        if !counted.insert((composer.as_str(), work.key)) {
            continue;
        }
        let counts = counts.entry(composer).or_default();
        for catalogue in numbers.iter().map(|n| n.catalogue).unique() {
            *counts.entry(catalogue).or_default() += 1;
        }
    }
    let main: HashMap<&str, &'static str> = counts
        .into_iter()
        .filter_map(|(composer, counts)| Some((composer, most_counted(counts)?)))
        .collect();

    works
        .iter()
        .zip(numbers)
        .zip(&composers)
        .map(|((work, numbers), composer)| {
            let main = main.get(composer.as_str());
            let catalogue = match numbers.iter().find(|n| Some(&n.catalogue) == main) {
                Some(number) => CatalogueOrder::Main(number.order.clone()),
                None => numbers
                    .into_iter()
                    .next()
                    .map_or(CatalogueOrder::None, CatalogueOrder::Other),
            };
            WorkOrder {
                no_composers: work.composers.is_empty(),
                first_composer: work
                    .composers
                    .first()
                    .map(|composer| glib::CollationKey::from(composer.sort.as_str())),
                composers: composer.clone(),
                catalogue,
                name: glib::FilenameCollationKey::from(work.name),
            }
        })
        .collect()
}

// The key of the work of a track, if it's part of one in the library,
// without the tracks in the `hidden` folders; the Works page lists an
// arrangement under the work it arranges, see `listed_under()`
pub(crate) fn track_work(path: &Path, hidden: &[PathBuf]) -> Option<String> {
    query_or_default("look up the work of a track", |conn| {
        query_track_work(conn, path, hidden)
    })
}

pub(super) fn query_track_work(
    conn: &Connection,
    path: &Path,
    hidden: &[PathBuf],
) -> rusqlite::Result<Option<String>> {
    // Only a work with tracks outside the hidden folders is listed
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    conn.query_row(
        &format!(
            "SELECT work_key FROM tracks AS track
             WHERE path = ? AND work_key IS NOT NULL AND EXISTS (
                 SELECT 1 FROM tracks
                 WHERE work_key = track.work_key AND album_id IS NOT NULL{hidden_sql})"
        ),
        rusqlite::params_from_iter(std::iter::once(path_bytes(path).to_vec()).chain(hidden_params)),
        |row| row.get(0),
    )
    .optional()
}

// The recordings of a work that the filter matches, and those of the
// arrangements listed under it, `keys` being its key and theirs, see
// `WorkInfo::keys()`, with all their tracks, see `recordings_of_works()`,
// in the order they were recorded, as the Recorded sort of the albums,
// those without a date last, and then by the albums' titles. The tracks
// in the `hidden` folders are left out
pub(crate) fn work_recordings(
    keys: &[String],
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> Vec<WorkRecording> {
    query_or_default("list the recordings of a work", |conn| {
        query_work_recordings(conn, keys, filter, hidden)
    })
}

pub(super) fn query_work_recordings(
    conn: &Connection,
    keys: &[String],
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkRecording>> {
    let rows = query_work_rows(conn, filter, keys, hidden, Lengths::Skip)?;
    let (work, arrangements) = keys.split_first().unzip();
    let listed: HashMap<&str, &str> = arrangements
        .into_iter()
        .flatten()
        .zip(std::iter::repeat(
            work.map(String::as_str).unwrap_or_default(),
        ))
        .map(|(key, work)| (key.as_str(), work))
        .collect();
    let mut dates = RecordingDates::new(conn);
    let mut recordings: Vec<(_, WorkRecording)> =
        recordings_of_works(&rows, |row| split_recording_work(row, &listed), &mut dates)
            .into_iter()
            .map(|recording| recording.into_iter().map(|i| &rows[i]).collect::<Vec<_>>())
            .filter(|rows| rows.iter().any(|row| row.matching))
            .map(|rows| {
                let album = rows.iter().find_map(|row| row.album.as_deref());
                let collation = glib::CollationKey::from(album.unwrap_or_default());
                let recorded = rows
                    .iter()
                    .map(|row| Earliest(row.recorded))
                    .min()
                    .unwrap_or(Earliest(None));
                let recording = WorkRecording {
                    album_id: rows[0].album_id.clone(),
                    year: recorded.0.map(|date| format!("{:04}", date.year)),
                    performers: performers_line(
                        rows.iter().filter_map(|row| row.performers.as_deref()),
                    ),
                    paths: rows.iter().map(|row| row.path.clone()).collect(),
                    work_key: rows[0].work.clone(),
                    arrangement: recording_arrangement(&rows, |work| listed.contains_key(work)),
                };
                ((recorded, collation), recording)
            })
            .collect();
    // In the order they were recorded, to the day where the tags say, those
    // without a date last, then by album
    recordings.sort_by(|((a_recorded, a_album), a), ((b_recorded, b_album), b)| {
        (a_recorded, a_album, &a.album_id).cmp(&(b_recorded, b_album, &b.album_id))
    });
    Ok(recordings
        .into_iter()
        .map(|(_, recording)| recording)
        .collect())
}

// Whether `query_work_rows()` reads the lengths of the tracks, which only
// Play a Random Work weighs the works by: reading them takes the `fields`
// of every track, which the Works page doesn't need
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Lengths {
    Read,
    Skip,
}

// The tracks of the works, or of the works of `keys`, in the order of
// their albums; the other tracks of the albums don't split the recordings
// of a work, see `recordings_of_works()`
pub(super) fn query_work_rows(
    conn: &Connection,
    filter: &TrackFilter,
    keys: &[String],
    hidden: &[PathBuf],
    lengths: Lengths,
) -> rusqlite::Result<Vec<WorkRow>> {
    let key_sql = if keys.is_empty() {
        String::new()
    } else {
        format!(" AND work_key IN ({})", placeholders(keys.len()))
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, path, work_key, movement_order, {},
                work, work_title, work_collection, work_composers, album, recorded_sort_date,
                performers,
                disc, track, work_arrangement_of, work_arrangement, work_arrangement_of_id,
                work_part_ids, {}
         FROM tracks
         WHERE album_id IS NOT NULL AND work_key IS NOT NULL{}{key_sql}",
        filter.matches_sql(),
        match lengths {
            Lengths::Read => "dismas_length(fields)",
            Lengths::Skip => "NULL",
        },
        hidden_condition("file", hidden).0
    ))?;
    let mut params = filter_values(filter, hidden);
    params.extend(
        keys.iter()
            .map(|key| Param::from(rusqlite::types::Value::Text(key.clone()))),
    );
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        let order = row.get::<_, Option<String>>(3)?.map(|order| {
            order
                .split('.')
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()
                .unwrap_or_default()
        });
        let path = path_from_bytes(row.get(1)?);
        let track_order = TrackOrder::new(&path, row.get(12)?, row.get(13)?);
        let row = WorkRow {
            album_id: row.get(0)?,
            path,
            work: row.get(2)?,
            order,
            matching: row.get(4)?,
            name: row.get(5)?,
            title: row.get(6)?,
            collection: row.get(7)?,
            composers: row.get(8)?,
            album: row.get(9)?,
            recorded: row
                .get::<_, Option<String>>(10)?
                .as_deref()
                .and_then(TagDate::from_sort_text),
            performers: row.get(11)?,
            arrangement_of: row.get(14)?,
            arrangement: row.get(15)?,
            arrangement_of_id: row.get(16)?,
            part_ids: row
                .get::<_, Option<String>>(17)?
                .map(|ids| ids.lines().map(ToString::to_string).collect())
                .unwrap_or_default(),
            length: row
                .get::<_, Option<i64>>(18)?
                .and_then(|length| u64::try_from(length).ok()),
        };
        Ok((track_order, row))
    })?;
    let mut rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    rows.sort_by(|(a_order, a), (b_order, b)| (&a.album_id, a_order).cmp(&(&b.album_id, b_order)));
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

// The recordings of the works, as the indices of their rows, which are in
// the order of their albums: the tracks of an album with the same work,
// which `work_of` gives for each row.
// They're not always in the order of the work, as in The Art of Fugue, nor
// next to each other, as in recitals that mix the pieces of a collection
// with others, and MusicBrainz links some of the tracks of an opera to the
// whole opera, or to an act, rather than to its scenes, so they make one
// recording. A movement that comes again starts another recording only
// when its tags show another performance, see `another_performance()`: a
// movement comes again far more often in a single performance, as a da
// capo, a variant of it or another version of the work after it, or a
// track that MusicBrainz links to the wrong movement, and splitting there
// broke operas into fragments. The parts of a movement in several tracks
// have its place, one after the other, and never split.
// Each work has one recording open on an album, the last one, so a track
// that goes back to an earlier performance without coming again in the
// last one joins the last one: performances that take turns, movement by
// movement, or more of the first after part of a second
fn recordings_of_works<'a>(
    rows: &'a [WorkRow],
    work_of: impl Fn(&'a WorkRow) -> &'a str,
    dates: &mut RecordingDates,
) -> Vec<Vec<usize>> {
    // For each work of the album being read, its last recording, and the
    // places of the movements in it
    struct Open<'a> {
        recording: usize,
        places: HashSet<&'a [u32]>,
    }

    let mut recordings: Vec<Vec<usize>> = Vec::new();
    let mut open: HashMap<&str, Open> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 && rows[i - 1].album_id != row.album_id {
            open.clear();
        }
        let work = work_of(row);
        let continues = open.get(work).is_some_and(|open| match &row.order {
            Some(place) if !place.is_empty() => {
                let recording = &recordings[open.recording];
                let last = &rows[*recording.last().unwrap()];
                last.order.as_ref() == Some(place)
                    || !open.places.contains(place.as_slice())
                    || !another_performance(rows, recording, row, dates)
            }
            _ => true,
        });
        if continues {
            let open = open.get_mut(work).unwrap();
            recordings[open.recording].push(i);
            if let Some(place) = row.order.as_ref().filter(|place| !place.is_empty()) {
                open.places.insert(place);
            }
        } else {
            recordings.push(vec![i]);
            let places = row
                .order
                .iter()
                .filter(|place| !place.is_empty())
                .map(Vec::as_slice)
                .collect();
            open.insert(
                work,
                Open {
                    recording: recordings.len() - 1,
                    places,
                },
            );
        }
    }
    recordings
}

// Whether the tags show that a track is another performance than the
// tracks of a `recording` on its album: it has no conductor or performer
// in common with them, or it was recorded more than a year away from all
// of them, see `RecordingDates`. What isn't known on either side shows
// nothing, so the track stays in the recording
fn another_performance(
    rows: &[WorkRow],
    recording: &[usize],
    row: &WorkRow,
    dates: &mut RecordingDates,
) -> bool {
    let theirs = || recording.iter().map(|&i| &rows[i]);
    let performers: HashSet<&str> = performer_keys(row.performers.as_deref()).collect();
    if !performers.is_empty() {
        let others: HashSet<&str> = theirs()
            .flat_map(|row| performer_keys(row.performers.as_deref()))
            .collect();
        if !others.is_empty() && performers.is_disjoint(&others) {
            return true;
        }
    }
    dates.read(&row.album_id);
    let recorded = dates.of(&row.path);
    if recorded.is_empty() {
        return false;
    }
    let others: Vec<(i64, i64)> = theirs()
        .flat_map(|row| dates.of(&row.path))
        .copied()
        .collect();
    !others.is_empty()
        && recorded
            .iter()
            .all(|a| others.iter().all(|b| years_apart(*a, *b)))
}

// Whether two times a date in the tags covers, see `tagged_span()`, are
// more than a year apart. Their ends are left out, so a gap of 365 days
// between the end of one and the start of the other is more than a year:
// 1990 and 1992 are, and so are a day and the same day two years later,
// but 1990 and 1991 aren't, nor a day and the same day a year later
fn years_apart(a: (i64, i64), b: (i64, i64)) -> bool {
    const YEAR: i64 = 365 * 24 * 60 * 60;
    a.0.max(b.0) - a.1.min(b.1) >= YEAR
}

// When the tracks of an album were recorded, from the RECORDINGDATE tags
// in their `fields`, see `another_performance()`. They're only read for an
// album where a movement comes again in a work, the one being read, as
// reading the fields of every track takes time the Works page doesn't
// need to spend, see `Lengths`. They're optional, so they're none when
// they can't be read, which keeps the tracks in one recording
pub(super) struct RecordingDates<'c> {
    conn: &'c Connection,
    album: Option<String>,
    // The times each track's dates cover, by path, for the tracks with any
    recorded: HashMap<PathBuf, Vec<(i64, i64)>>,
}

impl<'c> RecordingDates<'c> {
    pub(super) fn new(conn: &'c Connection) -> Self {
        Self {
            conn,
            album: None,
            recorded: HashMap::new(),
        }
    }

    // Reads the dates of the tracks of an album, unless they're read
    fn read(&mut self, album_id: &str) {
        if self.album.as_deref() == Some(album_id) {
            return;
        }
        self.album = Some(album_id.to_owned());
        self.recorded = query_recording_dates(self.conn, album_id).unwrap_or_else(|e| {
            warn!("Unable to read when the tracks of album {album_id} were recorded: {e}");
            HashMap::new()
        });
    }

    // The times the dates of a track of the album read cover
    fn of(&self, path: &Path) -> &[(i64, i64)] {
        self.recorded.get(path).map_or(&[], Vec::as_slice)
    }
}

fn query_recording_dates(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<HashMap<PathBuf, Vec<(i64, i64)>>> {
    let mut stmt = conn.prepare_cached(
        "SELECT path, fields FROM tracks
         WHERE album_id = ?1 AND work_key IS NOT NULL AND fields IS NOT NULL",
    )?;
    let rows = stmt.query_map([album_id], |row| {
        let fields: String = row.get(1)?;
        let recorded: Vec<(i64, i64)> = field_values(&fields, &[FieldCode::Recorded])
            .filter_map(tagged_span)
            .collect();
        Ok((path_from_bytes(row.get(0)?), recorded))
    })?;
    let mut recorded = HashMap::new();
    for row in rows {
        let (path, spans) = row?;
        if !spans.is_empty() {
            recorded.insert(path, spans);
        }
    }
    Ok(recorded)
}

// The composers of a work, from the lines of `composer_entries()`
fn work_composers(entries: &str) -> Vec<WorkComposer> {
    entries
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let key = fields.next()?.to_string();
            let name = fields.next()?.to_string();
            let sort = composer_sort(&name, fields.next()?);
            Some(WorkComposer { key, name, sort })
        })
        .collect()
}

// The conductors and performers of the tracks of a recording, from the
// lines of `performer_entries()`, each once: the conductors, then the
// soloists and the groups, as in the credits line of a track, e.g.
// "Philippe Herreweghe · Ilse Eerens, Collegium Vocale Gent". The tracks
// can name different ones, as the soloists only sing in some of the
// movements
fn performers_line<'a>(entries: impl Iterator<Item = &'a str>) -> String {
    let mut seen = HashSet::new();
    let mut conductors = Vec::new();
    let mut soloists = Vec::new();
    let mut groups = Vec::new();
    for (kind, key, name) in entries.flat_map(performer_lines) {
        if !seen.insert(key) {
            continue;
        }
        match kind {
            PerformerKind::Conductor => conductors.push(name),
            PerformerKind::Soloist => soloists.push(name),
            PerformerKind::Ensemble => groups.push(name),
            // Not given, see `performer_lines()`
            PerformerKind::Artist => {}
        }
    }
    let performers = soloists.into_iter().chain(groups).join(", ");
    [conductors.join(", "), performers]
        .into_iter()
        .filter(|s| !s.is_empty())
        .join(" · ")
}

// The kind, the key and the name of each conductor and performer in the
// lines of `performer_entries()`. Not the artists, which the tags of a
// whole work, as Picard gives a song, can have too, so that a recording
// is named, and told apart from others, by its credits as before
fn performer_lines(entries: &str) -> impl Iterator<Item = (PerformerKind, &str, &str)> {
    entries.lines().filter_map(|line| {
        let mut fields = line.split('\t');
        let kind = performer_kind_from_code(fields.next()?)?;
        if kind == PerformerKind::Artist {
            return None;
        }
        Some((kind, fields.next()?, fields.next()?))
    })
}

// The keys of the conductors and performers of a track, see
// `performer_entries()`, which tell them apart
fn performer_keys(entries: Option<&str>) -> impl Iterator<Item = &str> {
    entries
        .into_iter()
        .flat_map(performer_lines)
        .map(|(_, key, _)| key)
}

// The key of a work without a MusicBrainz id, see `work_entry()`: its name
// and its composers, ignoring case and accents
pub(crate) fn named_work_key(name: &str, composers: &[WorkComposer]) -> String {
    let composers = composers
        .iter()
        .map(|composer| composer.key.as_str())
        .sorted()
        .join("\x1f");
    format!("t:{}\x1f{composers}", composer_key(name))
}

#[cfg(test)]
mod tests {
    // The artist of a whole work, as Picard gives a song, neither names
    // its recording nor tells it apart from others
    #[test]
    fn artists_are_not_among_the_performers_of_a_recording() {
        let entries = "\nc\tvaclav luks\tVáclav Luks\tLuks, Václav\t\n\
                       a\tcollegium 1704\tCollegium 1704\tCollegium 1704\t\n";
        assert_eq!(
            super::performers_line(std::iter::once(entries)),
            "Václav Luks"
        );
        assert_eq!(
            super::performer_keys(Some(entries)).collect::<Vec<_>>(),
            ["vaclav luks"]
        );
    }
}
