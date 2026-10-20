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
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Instant, UNIX_EPOCH},
};

pub use album::{Album, AlbumSort};
use async_channel::Sender;
use gtk::{gio, glib, prelude::*};
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
    cover_image::CoverImage,
    i18n::i18n,
    utils,
};

// Bump when the schema or the way the tags are read changes, so that
// the whole library is read again
const SCHEMA_VERSION: i32 = 8;

// The size of the covers in the grid, twice the largest one for HiDPI
const THUMBNAIL_SIZE: u32 = 480;

pub enum ScanEvent {
    // The number of files read so far, out of the files that changed
    Progress(usize, usize),
    Finished,
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
        // albums that are gone or changed. Paths are kept as bytes, as
        // they are not always valid UTF-8
        debug!("Library schema version {version}, recreating it");
        tx.execute_batch(
            "DROP TABLE IF EXISTS tracks;
             CREATE TABLE tracks (
                 path BLOB PRIMARY KEY,
                 mtime INTEGER NOT NULL,
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
             CREATE TABLE IF NOT EXISTS covers (
                 album_id TEXT PRIMARY KEY,
                 path TEXT
             );",
        )?;
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
    if let Err(e) = scan_internal(music_dir, sender) {
        warn!("Unable to scan the library: {e}");
    }
    debug!("Library scan: {} ms", now.elapsed().as_millis());

    let _ = sender.try_send(ScanEvent::Finished);
}

fn scan_internal(music_dir: &Path, sender: &Sender<ScanEvent>) -> rusqlite::Result<()> {
    with_connection(|conn| scan_with(conn, music_dir, sender))
}

fn scan_with(
    conn: &mut Connection,
    music_dir: &Path,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<()> {
    let files = utils::load_files_from_folder(&gio::File::for_path(music_dir), true);

    // Find the files that are new or changed since the last scan
    let mut known = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, mtime, size FROM tracks")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?;
        for row in rows {
            let (path, stamp) = row?;
            known.insert(path, stamp);
        }
    }

    let mut present = HashSet::new();
    let mut changed = Vec::new();
    for file in files {
        let Some(path) = file.path() else {
            continue;
        };
        if !utils::is_audio_path(&path) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let seconds = |t: std::io::Result<std::time::SystemTime>| {
            t.ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        };
        let mtime = seconds(metadata.modified()).unwrap_or_default();
        let size = metadata.len() as i64;
        // When the file was created, so that the tracks that were in the
        // Music folder before the library existed are sorted by when they
        // were added too; it's only used for new tracks, as the time is
        // kept when a track changes
        let added = seconds(metadata.created()).unwrap_or(mtime);

        if known.get(path_bytes(&path)) != Some(&(mtime, size)) {
            changed.push((path.clone(), mtime, size, added));
        }
        present.insert(path_bytes(&path).to_vec());
    }

    debug!(
        "Library: {} tracks, {} new or changed",
        present.len(),
        changed.len()
    );

    // The albums whose cover should be looked for again
    let mut stale_albums = HashSet::new();

    let total = changed.len();
    for (i, chunk) in changed.chunks(200).enumerate() {
        let tx = conn.transaction()?;
        {
            let mut old_album =
                tx.prepare("SELECT album_id FROM tracks WHERE path = ?1 AND album_id IS NOT NULL")?;
            // A changed track keeps the time it was added
            let mut upsert = tx.prepare(
                "INSERT INTO tracks
                 (path, mtime, size, album_id, album, album_artist, artist, artist_sort,
                  year, disc, track, title, search, added)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT (path) DO UPDATE SET
                  mtime = excluded.mtime, size = excluded.size,
                  album_id = excluded.album_id, album = excluded.album,
                  album_artist = excluded.album_artist, artist = excluded.artist,
                  artist_sort = excluded.artist_sort, year = excluded.year,
                  disc = excluded.disc, track = excluded.track,
                  title = excluded.title, search = excluded.search",
            )?;
            for (path, mtime, size, added) in chunk {
                let key = path_bytes(path);
                if let Some(id) = old_album
                    .query_row([key], |row| row.get::<_, String>(0))
                    .optional()?
                {
                    stale_albums.insert(id);
                }

                // A changed track might have new cover art; new tracks
                // don't make the cover of their album stale, so that the
                // covers are kept when the whole library is read again
                let tags = read_tags(path);
                if let Some(id) = tags.album_id.as_ref().filter(|_| known.contains_key(key)) {
                    stale_albums.insert(id.clone());
                }

                upsert.execute(params![
                    key,
                    mtime,
                    size,
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
            }
        }
        tx.commit()?;

        let done = (i * 200 + chunk.len()).min(total);
        let _ = sender.try_send(ScanEvent::Progress(done, total));
    }

    // Forget the files that are gone
    let gone: Vec<Vec<u8>> = known
        .keys()
        .filter(|path| !present.contains(*path))
        .cloned()
        .collect();
    if !gone.is_empty() {
        debug!("Library: {} tracks removed", gone.len());
        let tx = conn.transaction()?;
        {
            let mut old_album =
                tx.prepare("SELECT album_id FROM tracks WHERE path = ?1 AND album_id IS NOT NULL")?;
            let mut delete = tx.prepare("DELETE FROM tracks WHERE path = ?1")?;
            for path in &gone {
                if let Some(id) = old_album
                    .query_row([path.as_slice()], |row| row.get::<_, String>(0))
                    .optional()?
                {
                    stale_albums.insert(id);
                }
                delete.execute([path.as_slice()])?;
            }
        }
        tx.commit()?;
    }

    update_covers(conn, &stale_albums)
}

// Creates the thumbnails of the albums that don't have one yet, or whose
// tracks changed; the thumbnails are in the cache directory, so they might
// have been removed since the last scan
fn update_covers(conn: &mut Connection, stale_albums: &HashSet<String>) -> rusqlite::Result<()> {
    let dir = covers_dir();
    let mut missing = Vec::new();
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
            // newer than the thumbnail, or the album had no cover before
            let cover_file_changed = match made {
                Some(Some(made)) => cover_file_time(&first_track).is_some_and(|t| t > made),
                Some(None) => false,
                None => cover_file_time(&first_track).is_some(),
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

// When the newest cover file in the folder of a track was changed, if it
// has any; the thumbnail is made from the first track of the album, which
// takes its cover from these files when its tags have none
fn cover_file_time(track: &Path) -> Option<std::time::SystemTime> {
    CoverCache::cover_files(track.parent()?)
        .filter_map(|file| std::fs::metadata(file).ok()?.modified().ok())
        .max()
}

fn create_thumbnail(track: &Path, dest: &Path) -> bool {
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

    let bytes = match tag {
        Some(tag) => CoverCache::load_cover_art(tag, track.parent()),
        None => None,
    };
    let Some(image) = bytes.and_then(|b| CoverImage::decode(b, THUMBNAIL_SIZE)) else {
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

fn read_tags(path: &Path) -> TrackTags {
    let mut options = ParseOptions::new();
    options.read_cover_art(false).read_properties(false);

    let probe = match Probe::open(path).map(|p| p.options(options).guess_file_type()) {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            warn!("Unable to parse file type {path:?}: {e}");
            return TrackTags::default();
        }
        Err(e) => {
            warn!("Unable to open {path:?}: {e}");
            return TrackTags::default();
        }
    };
    // The extra comments have the descriptions from YouTube, for searching
    let (tagged_file, extra) = match read_tagged_file(probe, options) {
        Ok(f) => f,
        Err(e) => {
            warn!("Unable to read {path:?}: {e}");
            return TrackTags::default();
        }
    };

    match tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())
    {
        Some(tag) => TrackTags::from_tag(tag, &extra, path),
        None => TrackTags::default(),
    }
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
}
