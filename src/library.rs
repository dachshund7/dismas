// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The albums in the music folders
//!
//! Reading the tags of every track takes a while, so they are kept in an
//! SQLite database in the user data directory, and only the files that
//! changed since the last scan are read again. The scan runs in a separate
//! thread, and reports its progress over a channel; the UI reads the albums
//! back from the database once it's done.

mod album;
mod covers;
mod db;
mod filter;
mod folders;
mod people;
mod scan;
mod watch;
mod works;

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, UNIX_EPOCH},
};

pub(crate) use album::{Album, AlbumSort, SelectionKey};
use async_channel::Sender;
pub(crate) use covers::*;
use db::*;
pub(crate) use filter::*;
pub(crate) use folders::*;
use gtk::{gio, gio::prelude::*, glib};
use itertools::Itertools;
use lofty::{
    prelude::Accessor,
    tag::{ItemKey, Tag},
};
use log::{debug, warn};
pub(crate) use people::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
pub(crate) use scan::*;
use sha2::{Digest, Sha256};
pub(crate) use watch::FolderWatch;
pub(crate) use works::*;

use crate::{
    audio::{
        Arrangement, AudioProperties, CoverCache, ExtraComments, FieldCode, FieldParts, FileTags,
        ListedPerformer, PerformerKind, ReadOptions, TrackCredits, field_values,
        first_and_last_names, has_name, normalize_key_text, normalize_search_text, plain_commas,
        read_file_tags, search_key, title_from_file_name, track_artist, track_search_text,
        track_title,
    },
    catalogue::{self, CatalogueNumber},
    cover_image::CoverImage,
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::i18n,
    play_stats,
    search_query::{Candidate, Context, Mode, Needs, Query, tagged_span},
    tag_date::{self, DateKind, DateSource, Earliest, SortDates, TagDate, TagDates},
    utils,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AlbumInfo {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    // The key the artist is sorted by
    pub(crate) artist_sort: String,
    // The year of its Recorded date, which is shown with it, see
    // `TrackCredits::year()`
    pub(crate) year: Option<String>,
    // The dates it's sorted by, see `SortDates`: of each kind, the earliest
    // its tracks have under the kind's own tag, or else under the next tag
    // of the chain that any of them has, see `earliest_sql()`
    pub(crate) dates: SortDates,
    // When the first track of the album was added, in seconds since the
    // epoch
    pub(crate) added: Option<i64>,
    pub(crate) cover: Option<String>,
    // The record label, and the number it gives the release
    pub(crate) label: Option<String>,
    pub(crate) catalog_number: Option<String>,
}

// The albums, sorted by artist, then when they were recorded and by title;
// the grid sorts them again in the order the user picked. The tracks in
// the `hidden` folders, which aren't there, are left out
pub(crate) fn albums(hidden: &[PathBuf]) -> Vec<AlbumInfo> {
    query_or_default("load the albums", |conn| query_albums(conn, hidden))
}

// The column of `tracks` with a track's date from `source`, see `TagDates`
fn date_column(source: DateSource) -> &'static str {
    match source {
        DateSource::Release => "release_sort_date",
        DateSource::Date => "date_tag_sort_date",
        DateSource::Original => "original_sort_date",
        DateSource::Recorded => "recorded_sort_date",
    }
}

// The date of `kind` of the tracks of a group, the `t` rows, as
// `TagDates::earliest()` works it out: the earliest under the kind's own
// tag, which `TagDate::sort_text()` orders as the sorts do, or else under
// the next tag of `tag_date::chain()` that any track has
fn earliest_sql(kind: DateKind) -> String {
    let earliest = tag_date::chain(kind)
        .map(|source| format!("MIN(t.{})", date_column(source)))
        .join(", ");
    format!("COALESCE({earliest})")
}

fn query_albums(conn: &Connection, hidden: &[PathBuf]) -> rusqlite::Result<Vec<AlbumInfo>> {
    let (hidden_sql, hidden_params) = hidden_condition("t.file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT t.album_id, MAX(t.album),
                COALESCE(MAX(t.album_artist),
                         CASE WHEN COUNT(DISTINCT t.artist) > 1 THEN NULL
                              ELSE MAX(t.artist) END),
                {recorded}, c.path, MAX(t.artist_sort), MIN(t.added),
                COUNT(DISTINCT t.artist), MAX(t.label), MAX(t.catalog_number),
                {released}, {original}
         FROM tracks t LEFT JOIN covers c ON c.album_id = t.album_id
         WHERE t.album_id IS NOT NULL{hidden_sql}
         GROUP BY t.album_id",
        recorded = earliest_sql(DateKind::Recorded),
        released = earliest_sql(DateKind::Released),
        original = earliest_sql(DateKind::OriginalRelease),
    ))?;
    // Without an album artist, the albums whose tracks are by different
    // artists are by the one of most of them, as a track "feat." another
    // artist doesn't make a compilation, or else compilations; those whose
    // tracks have none are by an unknown artist
    let mut main_artists = main_artists(conn, hidden)?;
    let various = i18n("Various Artists");
    let unknown = i18n("Unknown artist");
    let rows = stmt.query_map(rusqlite::params_from_iter(hidden_params), |row| {
        let artist = match row.get::<_, Option<String>>(2)? {
            Some(artist) => artist,
            None if row.get::<_, i64>(7)? == 0 => unknown.clone(),
            None => main_artists
                .remove(&row.get::<_, String>(0)?)
                .unwrap_or_else(|| various.clone()),
        };
        let sort_key = row
            .get::<_, Option<String>>(5)?
            .unwrap_or_else(|| artist.clone());
        let key = glib::CollationKey::from(sort_key.as_str());
        let date = |i| {
            row.get::<_, Option<String>>(i)
                .map(|date| date.as_deref().and_then(TagDate::from_sort_text))
        };
        let recorded = date(3)?;
        let info = AlbumInfo {
            id: row.get(0)?,
            title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            artist,
            artist_sort: sort_key,
            year: recorded.map(|date| format!("{:04}", date.year)),
            dates: SortDates {
                date: date(10)?,
                original: date(11)?,
                recorded,
            },
            added: row.get(6)?,
            cover: row.get(4)?,
            label: row.get(8)?,
            catalog_number: row.get(9)?,
        };
        let key = (key, glib::CollationKey::from(info.title.as_str()));
        Ok((key, info))
    })?;

    // Sorted by artist, then when they were recorded and by title, in the
    // order of the current locale
    let mut albums = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    albums.sort_by(|((a_artist, a_title), a), ((b_artist, b_title), b)| {
        let recorded = |info: &AlbumInfo| Earliest(info.dates.recorded);
        (a_artist, recorded(a), a_title).cmp(&(b_artist, recorded(b), b_title))
    });

    Ok(albums.into_iter().map(|(_, info)| info).collect())
}

// The artist of more than half of the tracks of each album, for the albums
// that have one
fn main_artists(
    conn: &Connection,
    hidden: &[PathBuf],
) -> rusqlite::Result<HashMap<String, String>> {
    let (hidden_sql, hidden_params) = hidden_condition("t.file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, artist FROM (
             SELECT t.album_id, t.artist, COUNT(*) AS n,
                    SUM(COUNT(*)) OVER (PARTITION BY t.album_id) AS total
             FROM tracks t
             WHERE t.album_id IS NOT NULL{hidden_sql}
             GROUP BY t.album_id, t.artist)
         WHERE artist IS NOT NULL AND 2 * n > total"
    ))?;
    stmt.query_map(rusqlite::params_from_iter(hidden_params), |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?
    .collect()
}

// The albums with a track that the filter matches, like searching the
// queue, with the number of their tracks that it does and the number of all
// their tracks; the tracks in the `hidden` folders are left out, as in
// `albums()`
pub(crate) fn search_albums(
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> HashMap<String, (u32, u32)> {
    query_or_default("search the albums", |conn| {
        query_search_albums(conn, filter, hidden)
    })
}

fn query_search_albums(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<HashMap<String, (u32, u32)>> {
    // For a work, only the albums of its tracks, which its index finds,
    // rather than all of them
    let of_work = if filter.works.is_empty() {
        String::new()
    } else {
        format!(
            " AND album_id IN (SELECT album_id FROM tracks WHERE work_key IN ({}))",
            placeholders(filter.works.len())
        )
    };
    let works = filter
        .works
        .iter()
        .map(|key| Param::from(rusqlite::types::Value::Text(key.clone())));
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, SUM({}) AS matching, COUNT(*)
         FROM tracks
         WHERE album_id IS NOT NULL{}{of_work}
         GROUP BY album_id
         HAVING matching > 0",
        filter.matches_sql(),
        hidden_condition("file", hidden).0
    ))?;
    let values = filter_values(filter, hidden).into_iter().chain(works);
    stmt.query_map(rusqlite::params_from_iter(values), |row| {
        Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
    })?
    .collect()
}

// The albums of the grid, by their ids, with the paths of their tracks in
// their order, without the tracks in the hidden folders
pub(crate) fn album_track_paths(hidden: &[PathBuf]) -> Vec<(String, Vec<PathBuf>)> {
    query_or_default("load the tracks of the albums", |conn| {
        query_album_track_paths(conn, hidden)
    })
}

// The title of an album, as `albums()` gives it, without the tracks in the
// `hidden` folders
pub(crate) fn album_title(album_id: &str, hidden: &[PathBuf]) -> Option<String> {
    query_or_default("look up the title of an album", |conn| {
        use rusqlite::types::Value;
        let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
        conn.query_row(
            &format!("SELECT MAX(album) FROM tracks WHERE album_id = ?{hidden_sql}"),
            rusqlite::params_from_iter(
                std::iter::once(Value::Text(album_id.to_owned()))
                    .chain(hidden_params.into_iter().map(Value::Blob)),
            ),
            |row| row.get(0),
        )
    })
}

fn query_album_track_paths(
    conn: &Connection,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<(String, Vec<PathBuf>)>> {
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, path, disc, track FROM tracks
         WHERE album_id IS NOT NULL{hidden_sql}
         ORDER BY album_id"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(hidden_params), |row| {
        let path = path_from_bytes(row.get::<_, Vec<u8>>(1)?);
        let order = TrackOrder::new(&path, row.get(2)?, row.get(3)?);
        Ok((row.get::<_, String>(0)?, (order, path)))
    })?;
    let mut albums: Vec<(String, Vec<(TrackOrder, PathBuf)>)> = Vec::new();
    for row in rows {
        let (id, track) = row?;
        match albums.last_mut() {
            Some((last, tracks)) if *last == id => tracks.push(track),
            _ => albums.push((id, vec![track])),
        }
    }
    Ok(albums
        .into_iter()
        .map(|(id, mut tracks)| {
            tracks.sort_by(|a, b| a.0.cmp(&b.0));
            (id, tracks.into_iter().map(|(_, path)| path).collect())
        })
        .collect())
}

// The order of the tracks of an album: by disc, and without a disc number
// by the folder, as the discs of an album are often in folders of their
// own whose track numbers start again, then by track number, and then by
// file name in the order a file manager shows them, "2" before "10"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TrackOrder {
    disc: i64,
    folder: Option<(bool, glib::FilenameCollationKey)>,
    track: i64,
    name: (bool, glib::FilenameCollationKey),
}

impl TrackOrder {
    pub(crate) fn new(path: &Path, disc: Option<i64>, track: Option<i64>) -> Self {
        let name = |path: Option<&Path>| {
            utils::file_name_key(&path.map(Path::to_string_lossy).unwrap_or_default())
        };
        Self {
            disc: disc.unwrap_or(0),
            folder: disc.is_none().then(|| name(path.parent())),
            track: track.unwrap_or(0),
            name: name(path.file_name().map(Path::new)),
        }
    }
}

// Whether a track is in an album, without the tracks in the `hidden`
// folders; with only the folders left out hidden, it says whether the
// music folders that are missing or empty have albums
pub(crate) fn has_albums(hidden: &[PathBuf]) -> bool {
    query_or_default("look for albums", |conn| {
        query_tracks_have(conn, "album_id", hidden)
    })
}

// The tracks the filter matches, as the Tracks toggle takes them from the
// albums, each with its title, if its tags give one; the tracks in the
// `hidden` folders are left out
pub(crate) fn matching_tracks(
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> Vec<(PathBuf, Option<String>)> {
    query_or_default("load the tracks the search finds", |conn| {
        query_matching_tracks(conn, filter, hidden)
    })
}

fn query_matching_tracks(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<(PathBuf, Option<String>)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT path, title FROM tracks WHERE album_id IS NOT NULL{}{}",
        filter.conditions_sql(),
        hidden_condition("file", hidden).0
    ))?;
    stmt.query_map(
        rusqlite::params_from_iter(filter_values(filter, hidden)),
        |row| Ok((path_from_bytes(row.get(0)?), row.get(1)?)),
    )?
    .collect()
}

// The paths of all the tracks of the library, to find where tracks moved,
// see `playlists::LibraryIndex`. This blocks
pub(crate) fn track_paths() -> Vec<PathBuf> {
    query_or_default("read the paths of the library's tracks", |conn| {
        conn.prepare("SELECT path FROM tracks")?
            .query_map([], |row| Ok(path_from_bytes(row.get(0)?)))?
            .collect()
    })
}

// What an exported playlist says of a track, in its #EXTINF line, as the
// library has it: the title of its tags, its first artist, and its length
// in seconds, if it's known
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ExportInfo {
    pub(crate) title: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) length: Option<u64>,
}

// What exported playlists say of the tracks the library has, by their
// paths, as the queue has them. This blocks
pub(crate) fn export_infos(paths: &[PathBuf]) -> HashMap<PathBuf, ExportInfo> {
    query_or_default("read the tracks of an exported playlist", |conn| {
        query_export_infos(conn, paths)
    })
}

fn query_export_infos(
    conn: &Connection,
    paths: &[PathBuf],
) -> rusqlite::Result<HashMap<PathBuf, ExportInfo>> {
    let mut stmt = conn.prepare_cached(
        "SELECT title, artist, dismas_length(fields) FROM tracks WHERE path = ?1",
    )?;
    let mut infos = HashMap::new();
    for path in paths {
        let info = stmt
            .query_row([path_bytes(path)], |row| {
                Ok(ExportInfo {
                    title: row.get(0)?,
                    artist: row.get(1)?,
                    length: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|length| u64::try_from(length).ok())
                        .filter(|length| *length > 0),
                })
            })
            .optional()?;
        if let Some(info) = info {
            infos.insert(path.clone(), info);
        }
    }
    Ok(infos)
}

// The files of the tracks of an album, in order; with a filter, only the
// tracks it matches, as in `search_albums()`. The tracks in the `hidden`
// folders are left out, as in `albums()`
pub(crate) fn album_tracks(
    album_id: &str,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> Vec<PathBuf> {
    query_or_default(
        format_args!("load the tracks of album {album_id}"),
        |conn| query_album_tracks(conn, album_id, filter, hidden),
    )
    .into_iter()
    .map(|(_, path)| path)
    .collect()
}

// All the files of the tracks of an album, as `album_tracks()` gives them,
// and whether they're on more than one disc
pub(crate) fn album_tracks_and_discs(album_id: &str, hidden: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let tracks = query_or_default(
        format_args!("load the tracks of album {album_id}"),
        |conn| query_album_tracks(conn, album_id, &TrackFilter::default(), hidden),
    );
    let discs = on_several_discs(&tracks);
    (tracks.into_iter().map(|(_, path)| path).collect(), discs)
}

// The tracks of an album, in order, with the disc numbers in their tags
fn query_album_tracks(
    conn: &Connection,
    album_id: &str,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<(Option<i64>, PathBuf)>> {
    let conditions = filter.conditions_sql();
    let mut stmt = conn.prepare(&format!(
        "SELECT path, disc, track FROM tracks
         WHERE album_id = ?{conditions}{}",
        hidden_condition("file", hidden).0
    ))?;
    let params = std::iter::once(Param::from(rusqlite::types::Value::Text(
        album_id.to_string(),
    )))
    .chain(filter_values(filter, hidden));
    let mut tracks = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            let path = path_from_bytes(row.get(0)?);
            let disc = row.get(1)?;
            Ok((TrackOrder::new(&path, disc, row.get(2)?), disc, path))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tracks.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(tracks
        .into_iter()
        .map(|(_, disc, path)| (disc, path))
        .collect())
}

// The disc of its album a track is on: its number in the tags, or else
// the disc folder it's in, as in Album/CD 2, by which `TrackOrder` puts its
// tracks in order; with neither, the first
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Disc {
    Number(u32),
    Folder(PathBuf),
}

impl Disc {
    // The disc of a track with the disc number `number`, in the folder `dir`
    pub(crate) fn of(number: Option<u32>, dir: Option<&Path>) -> Self {
        match (number, dir) {
            (Some(number), _) => Self::Number(number),
            (None, Some(dir)) if dir.file_name().is_some_and(is_disc_folder) => {
                Self::Folder(dir.to_path_buf())
            }
            _ => Self::Number(1),
        }
    }
}

// Whether tracks, with their disc numbers, are on more than one disc
fn on_several_discs(tracks: &[(Option<i64>, PathBuf)]) -> bool {
    !tracks
        .iter()
        .map(|(number, path)| {
            let number = number.and_then(|n| u32::try_from(n).ok());
            // The folder of the sheet, for its tracks
            Disc::of(number, cue_sheet::real_path(path).parent())
        })
        .all_equal()
}

// The album of a track, if it's part of the library and not in one of the
// `hidden` folders
pub(crate) fn track_album(path: &Path, hidden: &[PathBuf]) -> Option<String> {
    query_or_default(format_args!("look up the album of {path:?}"), |conn| {
        query_track_album(conn, path, hidden)
    })
}

fn query_track_album(
    conn: &Connection,
    path: &Path,
    hidden: &[PathBuf],
) -> rusqlite::Result<Option<String>> {
    use rusqlite::types::Value;

    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    conn.query_row(
        &format!("SELECT album_id FROM tracks WHERE path = ? AND album_id IS NOT NULL{hidden_sql}"),
        rusqlite::params_from_iter(
            std::iter::once(Value::Blob(path_bytes(path).to_vec()))
                .chain(hidden_params.into_iter().map(Value::Blob)),
        ),
        |row| row.get(0),
    )
    .optional()
}

// MusicBrainz ids are UUIDs; the album id names the file of its cover, so
// anything else in the tag, like a path, must not be used as one
pub(crate) fn is_mbid(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

// Whether a folder holds a disc of an album, e.g. "CD 1", "Disc 2 - Live"
// or "Disque 3", in a few languages: a word for a disc, then its number,
// of up to three digits, as box sets can have a hundred discs, but "Disco
// 1980" isn't one, and then anything after a space or a separator
pub(crate) fn is_disc_folder(name: &std::ffi::OsStr) -> bool {
    const WORDS: [&str; 7] = ["cd", "disc", "disk", "disco", "disque", "schijf", "płyta"];
    const SEPARATORS: [char; 4] = [' ', '_', '-', '.'];

    let name = name.to_string_lossy().to_lowercase();
    WORDS.iter().any(|word| {
        name.strip_prefix(word).is_some_and(|rest| {
            let rest = rest.trim_start_matches(SEPARATORS);
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            (1..=3).contains(&digits)
                && rest[digits..]
                    .chars()
                    .next()
                    .is_none_or(|c| SEPARATORS.contains(&c) || matches!(c, '(' | '['))
        })
    })
}

// The folder of the album that `dir` is a disc folder of, as Album is for
// Album/CD 1, where the album's cover and booklet are often left; `None`
// when it isn't a disc folder
pub(crate) fn disc_album_folder(dir: &Path) -> Option<&Path> {
    dir.parent()
        .filter(|_| dir.file_name().is_some_and(is_disc_folder))
}

// What stands for a track in the folder of its album, to be selected
// there, where `album_folder()` gives the folder itself: the disc folder
// it's in, as CD 1 is for Album/CD 1, or else its file. That's the sheet
// for a track of a sheet, the file the library keeps, rather than the
// audio file that the track menu selects, which would mean reading the
// sheet; the two are next to each other
pub(crate) fn album_folder_entry(path: &Path) -> &Path {
    let path = cue_sheet::real_path(path);
    match path.parent() {
        Some(dir) if disc_album_folder(dir).is_some() => dir,
        _ => path,
    }
}

// The folder of the album a track is in; the discs of an album are often
// in their own folders
pub(crate) fn album_folder(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    if is_disc_folder(parent.file_name()?) {
        parent.parent()
    } else {
        Some(parent)
    }
}

// The album of a track without an album tag: the folder it's in, by name
pub(crate) fn folder_album_title(path: &Path) -> Option<String> {
    album_folder(path)?
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests;
