// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The albums in the Music folder
//!
//! Reading the tags of every track takes a while, so they are kept in an
//! SQLite database in the user data directory, and only the files that
//! changed since the last scan are read again. The scan runs in a separate
//! thread, and reports its progress over a channel; the UI reads the albums
//! back from the database once it's done.

mod album;

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Instant, UNIX_EPOCH},
};

pub use album::{Album, AlbumSort};
use async_channel::Sender;
use gtk::{gio, glib};
use itertools::Itertools;
use lofty::{
    config::ParseOptions,
    prelude::{Accessor, TaggedFileExt},
    probe::Probe,
    tag::{ItemKey, Tag},
};
use log::{debug, warn};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{
    audio::{
        CoverCache, ExtraComments, normalize_search_text, plain_commas, read_tagged_file,
        tag_search_text,
    },
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::i18n,
    utils,
};

// Bump when the schema or the way the tags are read changes, so that
// the whole library is read again
const SCHEMA_VERSION: i32 = 10;

// The paths of the tracks are kept as bytes, as they are not always valid
// UTF-8. The path of a track of a CUE sheet is under its sheet, which is
// the file the track comes from, with the audio files the sheet splits;
// the times and the size are those of the files
const SCHEMA: &str = "DROP TABLE IF EXISTS tracks;
    CREATE TABLE tracks (
        path BLOB PRIMARY KEY,
        file BLOB NOT NULL,
        mtime INTEGER NOT NULL,
        ctime INTEGER NOT NULL,
        size INTEGER NOT NULL,
        added INTEGER NOT NULL,
        album_id TEXT,
        album TEXT,
        album_artist TEXT,
        artist TEXT,
        artist_sort TEXT,
        year TEXT,
        disc INTEGER,
        track INTEGER,
        title TEXT,
        search TEXT
    );
    CREATE INDEX tracks_album ON tracks (album_id);
    CREATE INDEX tracks_file ON tracks (file);
    CREATE TABLE IF NOT EXISTS covers (
        album_id TEXT PRIMARY KEY,
        path TEXT
    );";

// The size of the covers in the grid, twice the largest one for HiDPI
const THUMBNAIL_SIZE: u32 = 480;

pub enum ScanEvent {
    // The number of files read so far, out of the files that changed
    Progress(usize, usize),
    Finished(ScanResult),
}

#[derive(Debug, Default)]
pub struct ScanResult {
    // The tracks that were already known and changed, so that the queue
    // can read their tags again
    pub updated: Vec<PathBuf>,
    // The number of tracks that are new, changed or gone
    pub n_changed: usize,
}

#[derive(Debug, Clone)]
pub struct AlbumInfo {
    pub id: String,
    pub title: String,
    pub artist: String,
    // The key the artist is sorted by
    pub artist_sort: String,
    pub year: Option<String>,
    // When the first track of the album was added, in seconds since the
    // epoch
    pub added: Option<i64>,
    pub cover: Option<String>,
}

// The tags we keep for each track
#[derive(Default)]
struct TrackTags {
    album_id: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    artist: Option<String>,
    artist_sort: Option<String>,
    year: Option<String>,
    disc: Option<u32>,
    track: Option<u32>,
    title: Option<String>,
    // The text that searching looks into, as in the queue
    search: Option<String>,
}

// The XDG Music folder; without a user-dirs.dirs file, GLib doesn't know
// about one, so we look for the usual "Music" folder in the home directory
pub fn music_dir() -> Option<PathBuf> {
    glib::user_special_dir(glib::UserDirectory::Music)
        .filter(|dir| *dir != glib::home_dir())
        .or_else(|| Some(glib::home_dir().join("Music")))
        .filter(|dir| dir.is_dir())
}

fn data_dir() -> PathBuf {
    let mut dir = glib::user_data_dir();
    dir.push("dismas");
    dir
}

fn covers_dir() -> PathBuf {
    let mut dir = glib::user_cache_dir();
    dir.push("dismas");
    dir.push("album-covers");
    dir
}

// Each thread keeps its connection, rather than opening the database for
// each query: the scan has a thread of its own, and the queries of the UI
// run in the threads of GLib's pool
thread_local! {
    static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

fn with_connection<T>(
    query: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    CONNECTION.with_borrow_mut(|connection| {
        let conn = match connection {
            Some(conn) => conn,
            None => connection.insert(open()?),
        };
        query(conn)
    })
}

fn open() -> rusqlite::Result<Connection> {
    let dir = data_dir();
    glib::mkdir_with_parents(&dir, 0o755);

    let mut conn = Connection::open(dir.join("library.db"))?;
    // The scan writes while the UI reads
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(conn);
    }

    // Another thread can be opening the database too, so the version is
    // checked again once this one has the database to itself
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        // The tracks are read again; the covers of the albums are kept, as
        // they take a while to create, and the scan replaces the ones of the
        // albums that are gone or changed
        debug!("Library schema version {version}, recreating it");
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;

    Ok(conn)
}

// The paths of the tracks, as they're kept in the database
fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}

fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

// The albums, sorted by artist, then year and title; the grid sorts them
// again in the order the user picked
pub fn albums() -> Vec<AlbumInfo> {
    let query = |conn: &Connection| -> rusqlite::Result<Vec<AlbumInfo>> {
        let mut stmt = conn.prepare(
            "SELECT t.album_id, MAX(t.album),
                    COALESCE(MAX(t.album_artist),
                             CASE WHEN COUNT(DISTINCT t.artist) > 1 THEN NULL
                                  ELSE MAX(t.artist) END),
                    MIN(t.year), c.path, MAX(t.artist_sort), MIN(t.added)
             FROM tracks t LEFT JOIN covers c ON c.album_id = t.album_id
             WHERE t.album_id IS NOT NULL
             GROUP BY t.album_id",
        )?;
        // Without an album artist, the albums whose tracks are by different
        // artists are compilations
        let various = i18n("Various Artists");
        let rows = stmt.query_map([], |row| {
            let artist = row
                .get::<_, Option<String>>(2)?
                .unwrap_or_else(|| various.clone());
            let sort_key = row
                .get::<_, Option<String>>(5)?
                .unwrap_or_else(|| artist.clone());
            let key = glib::CollationKey::from(sort_key.as_str());
            let info = AlbumInfo {
                id: row.get(0)?,
                title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                artist,
                artist_sort: sort_key,
                year: row.get(3)?,
                added: row.get(6)?,
                cover: row.get(4)?,
            };
            let key = (
                key,
                info.year.clone(),
                glib::CollationKey::from(info.title.as_str()),
            );
            Ok((key, info))
        })?;

        // Sorted by artist, then year and title, in the order of the
        // current locale
        let mut albums = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        albums.sort_by(|(a, _), (b, _)| a.cmp(b));

        Ok(albums.into_iter().map(|(_, info)| info).collect())
    };

    match with_connection(|conn| query(conn)) {
        Ok(albums) => albums,
        Err(e) => {
            warn!("Unable to load the albums: {e}");
            Vec::new()
        }
    }
}

// The albums with a track that contains every word, which must be
// normalized with `normalize_search_text()`, like in the queue
pub fn search_albums(words: &[String]) -> HashSet<String> {
    let query = |conn: &Connection| -> rusqlite::Result<HashSet<String>> {
        let conditions = vec!["instr(search, ?) > 0"; words.len()].join(" AND ");
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT album_id FROM tracks
             WHERE album_id IS NOT NULL AND {conditions}"
        ))?;
        stmt.query_map(rusqlite::params_from_iter(words), |row| row.get(0))?
            .collect()
    };

    match with_connection(|conn| query(conn)) {
        Ok(albums) => albums,
        Err(e) => {
            warn!("Unable to search the albums: {e}");
            HashSet::new()
        }
    }
}

// The files of the tracks of an album, in order; with search words, only
// the tracks that contain every word, as in `search_albums()`
pub fn album_tracks(album_id: &str, words: &[String]) -> Vec<PathBuf> {
    let query = |conn: &Connection| -> rusqlite::Result<Vec<PathBuf>> {
        let conditions: String = words.iter().map(|_| " AND instr(search, ?) > 0").collect();
        let mut stmt = conn.prepare(&format!(
            "SELECT path FROM tracks
             WHERE album_id = ?{conditions}
             ORDER BY COALESCE(disc, 0), COALESCE(track, 0), path"
        ))?;
        let params = std::iter::once(album_id).chain(words.iter().map(String::as_str));
        stmt.query_map(rusqlite::params_from_iter(params), |row| {
            row.get::<_, Vec<u8>>(0).map(path_from_bytes)
        })?
        .collect()
    };

    match with_connection(|conn| query(conn)) {
        Ok(tracks) => tracks,
        Err(e) => {
            warn!("Unable to load the tracks of album {album_id}: {e}");
            Vec::new()
        }
    }
}

// The album of a track, if it's part of the library
pub fn track_album(path: &Path) -> Option<String> {
    let query = |conn: &Connection| -> rusqlite::Result<Option<String>> {
        conn.query_row(
            "SELECT album_id FROM tracks WHERE path = ?1 AND album_id IS NOT NULL",
            [path_bytes(path)],
            |row| row.get(0),
        )
        .optional()
    };

    match with_connection(|conn| query(conn)) {
        Ok(album_id) => album_id,
        Err(e) => {
            warn!("Unable to look up the album of {path:?}: {e}");
            None
        }
    }
}

// Updates the database with the tracks in `music_dir`; this blocks, so it
// should be called from a separate thread
pub fn scan(music_dir: &Path, sender: &Sender<ScanEvent>) {
    let now = Instant::now();
    let result = scan_internal(music_dir, sender).unwrap_or_else(|e| {
        warn!("Unable to scan the library: {e}");
        ScanResult::default()
    });
    debug!("Library scan: {} ms", now.elapsed().as_millis());

    let _ = sender.try_send(ScanEvent::Finished(result));
}

fn scan_internal(music_dir: &Path, sender: &Sender<ScanEvent>) -> rusqlite::Result<ScanResult> {
    with_connection(|conn| {
        let (result, stale_albums) = scan_with(conn, music_dir, sender)?;
        update_covers(conn, &stale_albums)?;
        Ok(result)
    })
}

// When the file of tracks last changed, and its size: with the audio
// files that a CUE sheet splits, the last change of any, and their sizes
// together
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stamp {
    mtime: i64,
    ctime: i64,
    size: i64,
}

// A file that tracks come from: an audio file, or a CUE sheet with the
// audio files it splits
struct Source {
    path: PathBuf,
    stamp: Stamp,
    // When it was created; a new track counts as added then, so that the
    // tracks that were in the Music folder before the library existed are
    // sorted by when they were added too
    created: i64,
    // The audio files a sheet splits
    audio: Vec<PathBuf>,
}

impl Source {
    fn new(path: PathBuf, audio: Vec<PathBuf>) -> Option<Self> {
        let seconds = |t: std::io::Result<std::time::SystemTime>| {
            t.ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        };
        let mut stamp = Stamp {
            mtime: 0,
            ctime: 0,
            size: 0,
        };
        let mut created = i64::MAX;
        for file in std::iter::once(&path).chain(&audio) {
            let metadata = std::fs::metadata(file).ok()?;
            let mtime = seconds(metadata.modified()).unwrap_or_default();
            stamp.mtime = stamp.mtime.max(mtime);
            stamp.ctime = stamp.ctime.max(change_time(&metadata));
            stamp.size += metadata.len() as i64;
            created = created.min(seconds(metadata.created()).unwrap_or(mtime));
        }
        Some(Self {
            path,
            stamp,
            created,
            audio,
        })
    }
}

// Updates the tracks, and returns the albums whose cover should be looked
// for again
fn scan_with(
    conn: &mut Connection,
    music_dir: &Path,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<(ScanResult, HashSet<String>)> {
    let paths: Vec<PathBuf> = utils::load_files_from_folder(&gio::File::for_path(music_dir), true)
        .iter()
        .filter_map(gio::prelude::FileExt::path)
        .collect();

    // The CUE sheets that split audio files have their tracks, in place
    // of the files
    let claims = Claims::new(paths.iter().map(PathBuf::as_path));

    // When each file changed at the last scan, and when its tracks were
    // added
    let mut known = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT file, mtime, ctime, size, MIN(added) FROM tracks GROUP BY file")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                Stamp {
                    mtime: row.get(1)?,
                    ctime: row.get(2)?,
                    size: row.get(3)?,
                },
                row.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (file, stamp, added) = row?;
            known.insert(file, (stamp, added));
        }
    }

    // Find the files that are new or changed since the last scan
    let mut present = HashSet::new();
    let mut changed = Vec::new();
    for path in paths {
        let audio: Vec<PathBuf> = if cue_sheet::is_cue_path(&path) {
            let audio: Vec<PathBuf> = claims
                .tracks_of(&path)
                .into_iter()
                .map(|(_, audio)| audio.to_path_buf())
                .unique()
                .collect();
            if audio.is_empty() {
                continue;
            }
            audio
        } else if utils::is_audio_path(&path) && claims.sheet_of(&path).is_none() {
            Vec::new()
        } else {
            continue;
        };
        let Some(source) = Source::new(path, audio) else {
            continue;
        };

        let key = path_bytes(&source.path).to_vec();
        if known.get(&key).map(|(stamp, _)| stamp) != Some(&source.stamp) {
            changed.push(source);
        }
        present.insert(key);
    }

    debug!(
        "Library: {} files of tracks, {} new or changed",
        present.len(),
        changed.len()
    );

    // The albums whose cover should be looked for again
    let mut stale_albums = HashSet::new();
    let mut updated = Vec::new();
    let mut n_changed = 0;

    let total = changed.len();
    for (i, chunk) in changed.chunks(200).enumerate() {
        let tx = conn.transaction()?;
        {
            let mut old_albums = tx.prepare(
                "SELECT DISTINCT album_id FROM tracks WHERE file = ?1 AND album_id IS NOT NULL",
            )?;
            let mut delete = tx.prepare("DELETE FROM tracks WHERE file = ?1")?;
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO tracks
                 (path, file, mtime, ctime, size, album_id, album, album_artist, artist,
                  artist_sort, year, disc, track, title, search, added)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            for source in chunk {
                let key = path_bytes(&source.path);
                for id in old_albums.query_map([key], |row| row.get::<_, String>(0))? {
                    stale_albums.insert(id?);
                }
                delete.execute([key])?;

                // A changed track keeps the time it was added, as do the
                // tracks of an audio file that a CUE sheet splits now
                let was_known = known.contains_key(key);
                let added = std::iter::once(&source.path)
                    .chain(&source.audio)
                    .filter_map(|path| known.get(path_bytes(path)))
                    .map(|(_, added)| *added)
                    .min()
                    .unwrap_or(source.created);

                let tracks = if source.audio.is_empty() {
                    read_tags(&source.path)
                } else {
                    claims
                        .sheets
                        .get(&source.path)
                        .map_or_else(Vec::new, |sheet| {
                            read_sheet_tags(sheet, claims.tracks_of(&source.path))
                        })
                };
                for (path, tags) in tracks {
                    // A changed track might have new cover art; new tracks
                    // don't make the cover of their album stale, so that the
                    // covers are kept when the whole library is read again
                    if let Some(id) = tags.album_id.as_ref().filter(|_| was_known) {
                        stale_albums.insert(id.clone());
                    }

                    insert.execute(params![
                        path_bytes(&path),
                        key,
                        source.stamp.mtime,
                        source.stamp.ctime,
                        source.stamp.size,
                        tags.album_id,
                        tags.album,
                        tags.album_artist,
                        tags.artist,
                        tags.artist_sort,
                        tags.year,
                        tags.disc,
                        tags.track,
                        tags.title,
                        tags.search,
                        added,
                    ])?;
                    n_changed += 1;
                    if was_known {
                        updated.push(path);
                    }
                }
            }
        }
        tx.commit()?;

        let done = (i * 200 + chunk.len()).min(total);
        let _ = sender.try_send(ScanEvent::Progress(done, total));
    }

    // Forget the files that are gone
    let gone: Vec<Vec<u8>> = known
        .keys()
        .filter(|file| !present.contains(*file))
        .cloned()
        .collect();
    if !gone.is_empty() {
        debug!("Library: {} files of tracks removed", gone.len());
        let tx = conn.transaction()?;
        {
            let mut old_albums = tx.prepare(
                "SELECT DISTINCT album_id FROM tracks WHERE file = ?1 AND album_id IS NOT NULL",
            )?;
            let mut delete = tx.prepare("DELETE FROM tracks WHERE file = ?1")?;
            for file in &gone {
                for id in old_albums.query_map([file.as_slice()], |row| row.get::<_, String>(0))? {
                    stale_albums.insert(id?);
                }
                n_changed += delete.execute([file.as_slice()])?;
            }
        }
        tx.commit()?;
    }

    Ok((ScanResult { updated, n_changed }, stale_albums))
}

// When the file last changed, in nanoseconds: unlike the modification
// time, taggers can't keep it from changing when they write the tags, as
// some do to keep the files in the order they were added
fn change_time(metadata: &std::fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata
            .ctime()
            .saturating_mul(1_000_000_000)
            .saturating_add(metadata.ctime_nsec())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

// Creates the thumbnails of the albums that don't have one yet, or whose
// tracks changed; the thumbnails are in the cache directory, so they might
// have been removed since the last scan
fn update_covers(conn: &mut Connection, stale_albums: &HashSet<String>) -> rusqlite::Result<()> {
    let dir = covers_dir();
    let mut missing = Vec::new();
    let mut cover_times = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT t.album_id, MIN(t.path), c.album_id, c.path
             FROM tracks t LEFT JOIN covers c ON c.album_id = t.album_id
             WHERE t.album_id IS NOT NULL
             GROUP BY t.album_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                path_from_bytes(row.get::<_, Vec<u8>>(1)?),
                row.get::<_, Option<String>>(2)?.is_some(),
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, first_track, has_row, cover) = row?;
            // Thumbnails of another size have another name, and are made
            // again
            let outdated = cover
                .as_deref()
                .is_some_and(|p| Path::new(p) != dir.join(thumbnail_name(&id)));
            // Albums without cover art have no thumbnail, so we only check
            // that the thumbnails we created are still there
            let made = cover.map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            let removed = matches!(made, Some(None));
            // A cover file next to the tracks can change without them: it's
            // newer than the thumbnail, or the album had no cover before. The
            // folder is read once, as albums can share one
            let mut cover_time = || {
                let dir = cue_sheet::real_path(&first_track).parent()?;
                *cover_times
                    .entry(dir.to_path_buf())
                    .or_insert_with(|| cover_file_time(dir))
            };
            let cover_file_changed = match made {
                Some(Some(made)) => cover_time().is_some_and(|t| t > made),
                Some(None) => false,
                None => cover_time().is_some(),
            };
            if !has_row || outdated || removed || cover_file_changed || stale_albums.contains(&id) {
                missing.push((id, first_track));
            }
        }
    }

    debug!("Library: generating {} album covers", missing.len());

    glib::mkdir_with_parents(&dir, 0o755);

    let tx = conn.transaction()?;
    {
        let mut upsert =
            tx.prepare("INSERT OR REPLACE INTO covers (album_id, path) VALUES (?1, ?2)")?;
        for (id, first_track) in missing {
            let path = dir.join(thumbnail_name(&id));
            let cover =
                create_thumbnail(&first_track, &path).then(|| path.to_string_lossy().to_string());
            upsert.execute(params![id, cover])?;
        }
        tx.execute(
            "DELETE FROM covers WHERE album_id NOT IN
             (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)",
            [],
        )?;
    }
    tx.commit()?;

    remove_unused_covers(conn, &dir)
}

fn thumbnail_name(album_id: &str) -> String {
    format!("{album_id}-{THUMBNAIL_SIZE}.png")
}

// Removes the thumbnails of the albums that are gone, or whose id changed,
// and those of another size
fn remove_unused_covers(conn: &Connection, dir: &Path) -> rusqlite::Result<()> {
    let used = conn
        .prepare("SELECT path FROM covers WHERE path IS NOT NULL")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()?;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "png")
            && !used.contains(path.to_string_lossy().as_ref())
        {
            debug!("Removing unused album cover {path:?}");
            if let Err(e) = std::fs::remove_file(&path) {
                warn!("Unable to remove album cover {path:?}: {e}");
            }
        }
    }

    Ok(())
}

// When the newest cover file in a folder of tracks was changed, if it
// has any; the thumbnail is made from the first track of the album, which
// takes its cover from these files when its tags have none
fn cover_file_time(dir: &Path) -> Option<std::time::SystemTime> {
    CoverCache::cover_files(dir)
        .filter_map(|file| std::fs::metadata(file).ok()?.modified().ok())
        .max()
}

fn create_thumbnail(track: &Path, dest: &Path) -> bool {
    let Some(track) = cue_sheet::audio_path(track) else {
        return false;
    };
    let track = track.as_path();
    // The type of the file is guessed from its contents, as the extension
    // can be wrong, e.g. Opus in an ".ogg" file
    let tagged_file = Probe::open(track)
        .ok()
        .and_then(|p| p.guess_file_type().ok())
        .and_then(|p| p.read().ok());
    let Some(tagged_file) = tagged_file else {
        return false;
    };
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    let Some(image) =
        tag.and_then(|tag| CoverCache::load_cover(tag, track.parent(), THUMBNAIL_SIZE))
    else {
        return false;
    };

    match image.save_png(dest) {
        Ok(_) => true,
        Err(e) => {
            warn!("Unable to save album cover {dest:?}: {e}");
            false
        }
    }
}

// An audio file's tags, and the items lofty leaves out of them, without
// the covers and properties, which the library doesn't keep
fn read_file_tags(path: &Path) -> Option<(lofty::file::TaggedFile, ExtraComments)> {
    let mut options = ParseOptions::new();
    options.read_cover_art(false).read_properties(false);

    let probe = match Probe::open(path).map(|p| p.options(options).guess_file_type()) {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            warn!("Unable to parse file type {path:?}: {e}");
            return None;
        }
        Err(e) => {
            warn!("Unable to open {path:?}: {e}");
            return None;
        }
    };
    // The extra comments have the descriptions from YouTube, for searching
    match read_tagged_file(probe, options) {
        Ok(f) => Some(f),
        Err(e) => {
            warn!("Unable to read {path:?}: {e}");
            None
        }
    }
}

// The tracks of an audio file: the file, or the tracks that the CUE sheet
// in its tags splits it into
fn read_tags(path: &Path) -> Vec<(PathBuf, TrackTags)> {
    let Some((tagged_file, extra)) = read_file_tags(path) else {
        return vec![(path.to_path_buf(), TrackTags::default())];
    };
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    let sheet = extra
        .cue_sheet
        .as_deref()
        .and_then(|text| SplitSheet::embedded(path, text));
    if let Some(sheet) = sheet {
        let album_tag = cue_sheet::album_tag(tag, &sheet.sheet);
        return sheet
            .tracks()
            .map(|(track, _)| cue_track_tags(&sheet, &album_tag, track))
            .collect();
    }

    let tags = tag.map_or_else(TrackTags::default, |tag| {
        TrackTags::from_tag(tag, &extra, path)
    });
    vec![(path.to_path_buf(), tags)]
}

// The tracks that a ".cue" file splits audio files into
fn read_sheet_tags(
    sheet: &SplitSheet,
    tracks: Vec<(&CueTrack, &Path)>,
) -> Vec<(PathBuf, TrackTags)> {
    let mut album_tags = HashMap::new();
    tracks
        .into_iter()
        .map(|(track, audio)| {
            let album_tag = album_tags.entry(audio).or_insert_with(|| {
                let file = read_file_tags(audio);
                let tag = file
                    .as_ref()
                    .and_then(|(f, _)| f.primary_tag().or_else(|| f.first_tag()));
                cue_sheet::album_tag(tag, &sheet.sheet)
            });
            cue_track_tags(sheet, album_tag, track)
        })
        .collect()
}

fn cue_track_tags(sheet: &SplitSheet, album_tag: &Tag, track: &CueTrack) -> (PathBuf, TrackTags) {
    let tag = cue_sheet::track_tag(album_tag, &sheet.sheet, track);
    // The album is in the folder of the sheet
    let tags = TrackTags::from_tag(&tag, &ExtraComments::default(), &sheet.path);
    (sheet.location(track), tags)
}

// MusicBrainz ids are UUIDs; the album id names the file of its cover, so
// anything else in the tag, like a path, must not be used as one
pub fn is_mbid(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

// The folder of the album a track is in; the discs of an album are often
// in their own folders, e.g. "CD 1" or "Disc 2"
fn album_folder(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    let name = parent.file_name()?.to_string_lossy().to_lowercase();
    let is_disc_folder = ["cd", "disc", "disk"].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|rest| {
            let rest = rest.trim_start_matches([' ', '_', '-', '.']);
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
        })
    });

    if is_disc_folder {
        parent.parent()
    } else {
        Some(parent)
    }
}

impl TrackTags {
    fn from_tag(tag: &Tag, extra: &ExtraComments, path: &Path) -> Self {
        let get = |key| tag.get_string(key).map(|s| s.trim().to_string());

        let album = get(ItemKey::AlbumTitle).filter(|s| !s.is_empty());
        let album_artist = get(ItemKey::AlbumArtist).filter(|s| !s.is_empty());
        let artist = tag
            .artist()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let artist_sort = get(ItemKey::AlbumArtistSortOrder).filter(|s| !s.is_empty());
        let year = get(ItemKey::RecordingDate)
            .or_else(|| get(ItemKey::Year))
            .or_else(|| get(ItemKey::ReleaseDate))
            .map(|s| s.chars().take(4).collect::<String>())
            .filter(|s| !s.is_empty());

        // Tracks without an album are not part of the grid. The release
        // id from MusicBrainz tells apart albums with the same name;
        // without it, we use the album artist, so the discs of an album in
        // separate folders stay together. Compilations often have no album
        // artist, and a different artist for each track, so we fall back to
        // the folder the album is in
        let album_id = album.as_ref().map(|album| {
            if let Some(mbid) = get(ItemKey::MusicBrainzReleaseId).filter(|s| is_mbid(s)) {
                mbid
            } else {
                let mut hasher = Sha256::new();
                hasher.update(album);
                hasher.update([0]);
                match (&album_artist, album_folder(path)) {
                    (Some(artist), _) => hasher.update(artist),
                    (None, Some(folder)) => {
                        hasher.update(folder.as_os_str().as_encoded_bytes());
                    }
                    (None, None) => (),
                }
                format!("{:x}", hasher.finalize())
            }
        });

        // The same text as `Track::search_key()`, so that searching the
        // albums matches the same tracks as searching the queue
        let title = tag.title().map(|s| s.to_string());
        let search = normalize_search_text(
            &[
                tag.get_strings(ItemKey::TrackArtist).join(", "),
                title.clone().unwrap_or_default(),
                tag_search_text(tag, extra),
            ]
            .join(" "),
        );

        TrackTags {
            album_id,
            album,
            // After the id, which must not change
            album_artist: album_artist.as_deref().map(plain_commas),
            artist: artist.as_deref().map(plain_commas),
            artist_sort,
            year,
            disc: tag.disk(),
            track: tag.track(),
            title,
            search: Some(search),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_mbid, path_bytes, path_from_bytes};

    // E.g. names from old Latin-1 rips
    #[cfg(unix)]
    #[test]
    fn paths_that_are_not_utf8_round_trip() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

        let path = Path::new(OsStr::from_bytes(b"/music/caf\xe9.flac"));
        assert_eq!(path_from_bytes(path_bytes(path).to_vec()), path);
    }

    #[test]
    fn only_uuids_are_musicbrainz_ids() {
        assert!(is_mbid("9e8f8d6e-3a1c-4b4f-9b0c-2f1d6a5e7c3b"));
        assert!(!is_mbid(""));
        assert!(!is_mbid("../../Pictures/photo"));
        assert!(!is_mbid("/home/user/Pictures/photo"));
    }

    // A track in the database, with paths from the music folder
    #[derive(Debug, PartialEq)]
    struct Row {
        path: String,
        file: String,
        track: Option<u32>,
        title: Option<String>,
        added: i64,
    }

    fn tracks(conn: &rusqlite::Connection, music: &std::path::Path) -> Vec<Row> {
        let relative = |bytes: Vec<u8>| {
            path_from_bytes(bytes)
                .strip_prefix(music)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let mut stmt = conn
            .prepare("SELECT path, file, track, title, added FROM tracks ORDER BY path")
            .unwrap();
        stmt.query_map([], |row| {
            Ok(Row {
                path: relative(row.get(0)?),
                file: relative(row.get(1)?),
                track: row.get(2)?,
                title: row.get(3)?,
                added: row.get(4)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    #[test]
    fn the_tracks_of_a_cue_sheet_take_the_place_of_its_file() {
        use super::{SCHEMA, scan_with};
        use crate::utils::write_test_wav;

        let music = std::env::temp_dir().join(format!("dismas-library-cue-{}", std::process::id()));
        std::fs::create_dir_all(music.join("Album")).unwrap();
        std::fs::create_dir_all(music.join("Other")).unwrap();
        write_test_wav(&music.join("Album/Album.wav"), 10);
        write_test_wav(&music.join("Other/Song.wav"), 1);
        let cue = music.join("Album/Album.cue");
        std::fs::write(
            &cue,
            "TITLE \"Album\"\nFILE \"Album.wav\" WAVE\n\
             TRACK 01 AUDIO\nTITLE \"One\"\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nTITLE \"Two\"\nINDEX 01 00:03:00\n",
        )
        .unwrap();

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let (sender, _receiver) = async_channel::unbounded();

        let (result, _) = scan_with(&mut conn, &music, &sender).unwrap();
        assert_eq!(result.n_changed, 3);
        let found = tracks(&conn, &music);
        let files: Vec<_> = found
            .iter()
            .map(|row| (row.path.as_str(), row.file.as_str()))
            .collect();
        assert_eq!(
            files,
            vec![
                ("Album/Album.cue/track01", "Album/Album.cue"),
                ("Album/Album.cue/track02", "Album/Album.cue"),
                ("Other/Song.wav", "Other/Song.wav"),
            ]
        );
        let titles: Vec<_> = found
            .iter()
            .map(|row| (row.track, row.title.as_deref()))
            .collect();
        assert_eq!(
            titles,
            vec![(Some(1), Some("One")), (Some(2), Some("Two")), (None, None)]
        );
        let added = found[0].added;

        // Nothing changed
        let (result, _) = scan_with(&mut conn, &music, &sender).unwrap();
        assert_eq!(result.n_changed, 0);

        // Without the sheet, the file is a track again, added when its
        // tracks were
        std::fs::remove_file(&cue).unwrap();
        let (result, _) = scan_with(&mut conn, &music, &sender).unwrap();
        assert_eq!(result.n_changed, 3);
        let found = tracks(&conn, &music);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "Album/Album.wav");
        assert_eq!(found[0].file, "Album/Album.wav");
        assert_eq!(found[0].added, added);

        std::fs::remove_dir_all(&music).unwrap();
    }
}
