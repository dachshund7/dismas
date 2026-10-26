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

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Instant, UNIX_EPOCH},
};

pub use album::{Album, AlbumSort};
use async_channel::Sender;
use gtk::{gio, gio::prelude::*, glib};
use itertools::Itertools;
use lofty::{
    config::ParseOptions,
    prelude::{Accessor, TaggedFileExt},
    tag::{ItemKey, Tag},
};
use log::{debug, warn};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{
    audio::{
        CoverCache, ExtraComments, file_tag, plain_commas, read_file_tags, search_key,
        tag_search_text, title_from_file_name, track_title,
    },
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::i18n,
    utils,
};

// Bump when the schema or the way the tags are read changes, so that
// the whole library is read again
const SCHEMA_VERSION: i32 = 13;

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
    // Whether covers of the albums were made again, which can keep their
    // names, as when a cover file next to the tracks changed
    pub covers_changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
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

// A folder whose albums are in the grid: the path that's read, and the
// path outside the sandbox of a folder that came through the document
// portal, which names it and tells it apart even when it's gone, e.g.
// when its access was taken back. The folders in it that are left out are
// relative to it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicFolder {
    pub path: PathBuf,
    pub host: Option<PathBuf>,
    pub excluded: Vec<PathBuf>,
}

impl MusicFolder {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            host: None,
            excluded: Vec::new(),
        }
    }

    // The path to show
    pub fn shown_path(&self) -> &Path {
        self.host.as_deref().unwrap_or(&self.path)
    }

    // The paths of the folders left out, as they're read
    pub fn excluded_paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.excluded
            .iter()
            .map(|relative| self.path.join(relative))
    }
}

// The XDG Music folder; without a user-dirs.dirs file, GLib doesn't know
// about one, so we look for the usual "Music" folder in the home directory
pub fn default_music_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Music)
        .filter(|dir| *dir != glib::home_dir())
        .unwrap_or_else(|| glib::home_dir().join("Music"))
}

// The music folders in the settings, where an empty list means the XDG
// Music folder, with the folders they leave out. They're kept as URIs, as
// the strings of the settings must be UTF-8; the ones without a local path
// are left out
pub fn music_folders(settings: &gio::Settings) -> Vec<MusicFolder> {
    folders_from_entries(
        settings.get::<Vec<(String, String)>>("music-folders"),
        settings.get::<Vec<String>>("excluded-folders"),
    )
}

fn folders_from_entries(entries: Vec<(String, String)>, excluded: Vec<String>) -> Vec<MusicFolder> {
    let mut folders: Vec<MusicFolder> = if entries.is_empty() {
        vec![MusicFolder::new(default_music_dir())]
    } else {
        entries
            .into_iter()
            .filter_map(|(uri, host)| {
                let path = gio::File::for_uri(&uri).path()?;
                let host = Some(host)
                    .filter(|host| !host.is_empty())
                    .and_then(|host| gio::File::for_uri(&host).path());
                Some(MusicFolder {
                    path,
                    host,
                    excluded: Vec::new(),
                })
            })
            .collect()
    };

    // A folder that's left out belongs to the music folder it's in; one
    // that isn't in any is forgotten when the folders are stored again
    for path in excluded
        .iter()
        .filter_map(|uri| gio::File::for_uri(uri).path())
    {
        let found = folders.iter_mut().find_map(|folder| {
            let relative = path
                .strip_prefix(&folder.path)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())?
                .to_path_buf();
            Some((folder, relative))
        });
        if let Some((folder, relative)) = found
            && !folder.excluded.contains(&relative)
        {
            folder.excluded.push(relative);
        }
    }
    folders
}

pub fn store_music_folders(settings: &gio::Settings, folders: &[MusicFolder]) {
    let uri = |path: &Path| gio::File::for_path(path).uri().to_string();
    let entries: Vec<(String, String)> = folders
        .iter()
        .map(|folder| {
            let host = folder.host.as_deref().map(uri).unwrap_or_default();
            (uri(&folder.path), host)
        })
        .collect();
    let excluded: Vec<String> = folders
        .iter()
        .flat_map(MusicFolder::excluded_paths)
        .map(|path| uri(&path))
        .collect();
    if let Err(e) = settings
        .set("music-folders", entries.to_variant())
        .and_then(|()| settings.set("excluded-folders", excluded.to_variant()))
    {
        warn!("Unable to store the music folders: {e}");
    }
}

// A folder picked in the file chooser, as it's read. The portal gives a
// folder of the Music folder a path of its own, as it grants write access,
// which the sandbox doesn't have; the Music folder's own path is read
// instead, spelled like the Music folder, so that its tracks have the
// same paths either way. This blocks, as the portal's files can be slow
pub fn resolve_folder(path: &Path) -> MusicFolder {
    let Some(host) = host_path(path) else {
        return MusicFolder::new(path.to_path_buf());
    };
    // The host path has its symbolic links resolved, as the Music folder
    // might have, e.g. to another drive
    let music = default_music_dir();
    if let Ok(real_music) = std::fs::canonicalize(&music)
        && let Ok(relative) = host.strip_prefix(&real_music)
        && music.join(relative).is_dir()
    {
        return MusicFolder::new(music.join(relative));
    }
    MusicFolder {
        host: Some(host),
        ..MusicFolder::new(path.to_path_buf())
    }
}

// The path outside the sandbox of a folder of the document portal
fn host_path(path: &Path) -> Option<PathBuf> {
    if !path.starts_with(glib::user_runtime_dir().join("doc")) {
        return None;
    }
    let info = gio::File::for_path(path)
        .query_info(
            utils::ATTRIBUTE_HOST_PATH,
            gio::FileQueryInfoFlags::NONE,
            None::<&gio::Cancellable>,
        )
        .ok()?;
    info.attribute_as_string(utils::ATTRIBUTE_HOST_PATH)
        .map(|host| PathBuf::from(host.as_str()))
}

// What tells folders apart, and whether one is in another: the path
// outside the sandbox, or the path without symbolic links, as two paths
// can lead to the same folder. This blocks
pub fn folder_key(folder: &MusicFolder) -> PathBuf {
    folder.host.clone().unwrap_or_else(|| {
        std::fs::canonicalize(&folder.path).unwrap_or_else(|_| folder.path.clone())
    })
}

// The folders to read, and those that aren't there, whose tracks are
// kept, as reading them again when they're back takes a while, but hidden,
// as they can't play. The folders left out of them aren't read, and their
// tracks are hidden until the scan forgets them
#[derive(Debug, Default)]
pub struct Roots {
    pub available: Vec<PathBuf>,
    pub unavailable: Vec<PathBuf>,
    pub excluded: Vec<PathBuf>,
}

// The music folders, without those in another one; this blocks
pub fn roots(folders: &[MusicFolder]) -> Roots {
    let keyed = folders
        .iter()
        .map(|folder| (folder_key(folder), folder))
        .collect();
    let mut roots = Roots::default();
    for folder in outermost(keyed) {
        roots.excluded.extend(folder.excluded_paths());
        if is_available(&folder.path) {
            roots.available.push(folder.path.clone());
        } else {
            roots.unavailable.push(folder.path.clone());
        }
    }
    roots
}

// The path of a folder relative to a music folder it's in, to leave it
// out; this blocks
pub fn relative_to(folder: &MusicFolder, music_folder: &MusicFolder) -> Option<PathBuf> {
    folder_key(folder)
        .strip_prefix(folder_key(music_folder))
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

// The values whose keys are neither in another's, nor the same as an
// earlier one's
fn outermost<T>(keyed: Vec<(PathBuf, T)>) -> Vec<T> {
    let keys: Vec<PathBuf> = keyed.iter().map(|(key, _)| key.clone()).collect();
    keyed
        .into_iter()
        .enumerate()
        .filter(|(i, (key, _))| {
            !keys
                .iter()
                .enumerate()
                .any(|(j, other)| key.starts_with(other) && (key != other || j < *i))
        })
        .map(|(_, (_, value))| value)
        .collect()
}

// Whether a folder can be read: an empty one counts as gone, as it's often
// where a drive is mounted, while it isn't
pub fn is_available(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

// Hidden files and folders aren't part of the library, like the trash of
// a drive, which keeps deleted files under their own names, and neither is
// Windows' recycle bin
fn is_library_entry(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return true;
    };
    let name = name.as_encoded_bytes();
    !name.starts_with(b".") && name != b"$RECYCLE.BIN"
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
    glib::mkdir_with_parents(&dir, 0o700);
    open_or_recreate(&dir.join("library.db"))
}

// The database is only a cache of the tags, so a damaged one is made again,
// rather than leaving the album grid empty for good; the files of the
// covers are kept
fn open_or_recreate(path: &Path) -> rusqlite::Result<Connection> {
    match open_at(path) {
        Err(e)
            if matches!(
                e.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
            ) =>
        {
            warn!("The library database is damaged, making it again: {e}");
            for suffix in ["", "-wal", "-shm"] {
                let mut file = path.as_os_str().to_owned();
                file.push(suffix);
                if let Err(e) = std::fs::remove_file(&file)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    warn!("Unable to remove {file:?}: {e}");
                }
            }
            open_at(path)
        }
        result => result,
    }
}

fn open_at(path: &Path) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
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

// A condition that leaves out the tracks whose file is in one of the
// folders, and its parameters: the paths in a folder sort between its path
// with a slash, and with the character after the slash, '0'
fn hidden_condition(column: &str, hidden: &[PathBuf]) -> (String, Vec<Vec<u8>>) {
    let mut sql = String::new();
    let mut params = Vec::new();
    for folder in hidden {
        let mut start = path_bytes(folder).to_vec();
        if start.last() != Some(&b'/') {
            start.push(b'/');
        }
        let mut end = start.clone();
        if let Some(last) = end.last_mut() {
            *last += 1;
        }
        sql.push_str(&format!(" AND NOT ({column} >= ? AND {column} < ?)"));
        params.push(start);
        params.push(end);
    }
    (sql, params)
}

// The albums, sorted by artist, then year and title; the grid sorts them
// again in the order the user picked. The tracks in the `hidden` folders,
// which aren't there, are left out
pub fn albums(hidden: &[PathBuf]) -> Vec<AlbumInfo> {
    match with_connection(|conn| query_albums(conn, hidden)) {
        Ok(albums) => albums,
        Err(e) => {
            warn!("Unable to load the albums: {e}");
            Vec::new()
        }
    }
}

fn query_albums(conn: &Connection, hidden: &[PathBuf]) -> rusqlite::Result<Vec<AlbumInfo>> {
    let (hidden_sql, hidden_params) = hidden_condition("t.file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT t.album_id, MAX(t.album),
                COALESCE(MAX(t.album_artist),
                         CASE WHEN COUNT(DISTINCT t.artist) > 1 THEN NULL
                              ELSE MAX(t.artist) END),
                MIN(t.year), c.path, MAX(t.artist_sort), MIN(t.added),
                COUNT(DISTINCT t.artist)
         FROM tracks t LEFT JOIN covers c ON c.album_id = t.album_id
         WHERE t.album_id IS NOT NULL{hidden_sql}
         GROUP BY t.album_id"
    ))?;
    // Without an album artist, the albums whose tracks are by different
    // artists are compilations, and those whose tracks have none are by an
    // unknown artist
    let various = i18n("Various Artists");
    let unknown = i18n("Unknown artist");
    let rows = stmt.query_map(rusqlite::params_from_iter(hidden_params), |row| {
        let artist = match row.get::<_, Option<String>>(2)? {
            Some(artist) => artist,
            None if row.get::<_, i64>(7)? == 0 => unknown.clone(),
            None => various.clone(),
        };
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
}

// The albums with a track that contains every word, which must be
// normalized with `normalize_search_text()`, like in the queue, with the
// number of their tracks that do and the number of all their tracks; the
// tracks in the `hidden` folders are left out, as in `albums()`
pub fn search_albums(words: &[String], hidden: &[PathBuf]) -> HashMap<String, (u32, u32)> {
    match with_connection(|conn| query_search_albums(conn, words, hidden)) {
        Ok(albums) => albums,
        Err(e) => {
            warn!("Unable to search the albums: {e}");
            HashMap::new()
        }
    }
}

fn query_search_albums(
    conn: &Connection,
    words: &[String],
    hidden: &[PathBuf],
) -> rusqlite::Result<HashMap<String, (u32, u32)>> {
    use rusqlite::types::Value;

    // A track without search text matches nothing
    let conditions = vec!["instr(search, ?) > 0"; words.len()].join(" AND ");
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, SUM(CASE WHEN {conditions} THEN 1 ELSE 0 END) AS matching,
                COUNT(*)
         FROM tracks
         WHERE album_id IS NOT NULL{hidden_sql}
         GROUP BY album_id
         HAVING matching > 0"
    ))?;
    let params = words
        .iter()
        .map(|word| Value::Text(word.clone()))
        .chain(hidden_params.into_iter().map(Value::Blob));
    stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
    })?
    .collect()
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

// Updates the database with the tracks in the music folders, and makes the
// covers of the albums for the grid `thumbnail_size` pixels wide; this
// blocks, so it should be called from a separate thread
pub fn scan(folders: &[MusicFolder], thumbnail_size: u32, sender: &Sender<ScanEvent>) {
    let now = Instant::now();
    let result = scan_internal(folders, thumbnail_size, sender).unwrap_or_else(|e| {
        warn!("Unable to scan the library: {e}");
        ScanResult::default()
    });
    debug!("Library scan: {} ms", now.elapsed().as_millis());

    let _ = sender.try_send(ScanEvent::Finished(result));
}

fn scan_internal(
    folders: &[MusicFolder],
    thumbnail_size: u32,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<ScanResult> {
    let roots = roots(folders);
    debug!("Library: reading {roots:?}");
    with_connection(|conn| {
        let (mut result, stale_albums) = scan_with(conn, &roots, sender)?;
        result.covers_changed =
            update_covers(conn, &stale_albums, &roots.unavailable, thumbnail_size)? > 0;
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
    // tracks that were in the music folders before the library existed, or
    // before their folder was added, are sorted by when they were added too
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
    roots: &Roots,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<(ScanResult, HashSet<String>)> {
    let paths: Vec<PathBuf> = roots
        .available
        .iter()
        .flat_map(|root| {
            utils::load_files_from_folder_filtered(&gio::File::for_path(root), true, &|path| {
                is_library_entry(path) && !roots.excluded.iter().any(|excluded| excluded == path)
            })
        })
        .filter_map(|file| file.path())
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

                // A file that makes the tag reader panic is kept as a track
                // without tags, which is in no album, so that the scan goes
                // on, and the file isn't read again until it changes
                let read = || {
                    if source.audio.is_empty() {
                        read_tags(&source.path)
                    } else {
                        claims
                            .sheets
                            .get(&source.path)
                            .map_or_else(Vec::new, |sheet| {
                                read_sheet_tags(sheet, claims.tracks_of(&source.path))
                            })
                    }
                };
                let tracks = utils::guarded(source.path.display(), read, || {
                    vec![(source.path.clone(), TrackTags::default())]
                });
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

    // Forget the files that are gone, or left out, but not those of the
    // folders that aren't there, e.g. on a drive that isn't plugged in, which
    // aren't read again once they're back
    let gone: Vec<Vec<u8>> = known
        .keys()
        .filter(|file| !present.contains(*file))
        .filter(|file| {
            let path = path_from_bytes(file.to_vec());
            !roots.unavailable.iter().any(|root| path.starts_with(root))
        })
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

    Ok((
        ScanResult {
            updated,
            n_changed,
            covers_changed: false,
        },
        stale_albums,
    ))
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
// have been removed since the last scan. They're made from the tracks that
// are there, so the albums of the `hidden` folders, which aren't, keep
// theirs until they're back. Returns the number of thumbnails made: one
// made again can keep its name, which the grid can't tell otherwise. An
// album whose cover can't be decoded is tried at every scan, and counts
// for nothing, as its thumbnail stays as it was, missing
fn update_covers(
    conn: &mut Connection,
    stale_albums: &HashSet<String>,
    hidden: &[PathBuf],
    size: u32,
) -> rusqlite::Result<usize> {
    let dir = covers_dir();
    let mut missing = Vec::new();
    let mut cover_times = HashMap::new();
    {
        let (hidden_sql, hidden_params) = hidden_condition("t.file", hidden);
        let mut stmt = conn.prepare(&format!(
            "SELECT t.album_id, MIN(t.path), c.album_id, c.path
             FROM tracks t LEFT JOIN covers c ON c.album_id = t.album_id
             WHERE t.album_id IS NOT NULL{hidden_sql}
             GROUP BY t.album_id"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(hidden_params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                path_from_bytes(row.get::<_, Vec<u8>>(1)?),
                row.get::<_, Option<String>>(2)?.is_some(),
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, first_track, has_row, cover) = row?;
            // Thumbnails smaller than the grid shows them, or in the format
            // of older versions, are made again. A larger one, made while a
            // monitor with a larger scale was connected, is kept, as the
            // monitors can change from one start to the next
            let outdated = cover.as_deref().is_some_and(|p| {
                thumbnail_size(Path::new(p), &dir, &id).is_none_or(|made| made < size)
            });
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
            // A track that's gone since the scan would leave its album
            // without a cover for good
            if (!has_row || outdated || removed || cover_file_changed || stale_albums.contains(&id))
                && cue_sheet::real_path(&first_track).is_file()
            {
                missing.push((id, first_track));
            }
        }
    }

    debug!("Library: generating {} album covers", missing.len());
    let mut n_made = 0;

    glib::mkdir_with_parents(&dir, 0o700);

    let tx = conn.transaction()?;
    {
        let mut upsert =
            tx.prepare("INSERT OR REPLACE INTO covers (album_id, path) VALUES (?1, ?2)")?;
        for (id, first_track) in missing {
            let made = utils::guarded(
                first_track.display(),
                || create_thumbnail(&first_track, &dir, &id, size),
                || None,
            );
            n_made += usize::from(made.is_some());
            let cover = made.map(|path| path.to_string_lossy().to_string());
            upsert.execute(params![id, cover])?;
        }
        tx.execute(
            "DELETE FROM covers WHERE album_id NOT IN
             (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)",
            [],
        )?;
    }
    tx.commit()?;

    remove_unused_covers(conn, &dir)?;
    Ok(n_made)
}

// The name of the thumbnail of an album: a JPEG, or a PNG for a cover with
// transparency, which JPEG can't keep. Thumbnails used to be PNG files
// named like the JPEGs, which these names tell apart from the PNG files made
// for transparency, so that the old ones are made again as JPEG
fn thumbnail_name(album_id: &str, size: u32, transparent: bool) -> String {
    if transparent {
        format!("{album_id}-{size}-alpha.png")
    } else {
        format!("{album_id}-{size}.jpg")
    }
}

// The size of the album's thumbnail, in either format, if the file is one
// that `thumbnail_name()` names
fn thumbnail_size(path: &Path, dir: &Path, album_id: &str) -> Option<u32> {
    if path.parent()? != dir {
        return None;
    }
    let size = path
        .file_name()?
        .to_str()?
        .strip_prefix(album_id)?
        .strip_prefix('-')
        .and_then(|rest| {
            rest.strip_suffix(".jpg")
                .or_else(|| rest.strip_suffix("-alpha.png"))
        })?;
    if size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    size.parse().ok()
}

// Removes the thumbnails of the albums that are gone, or whose id changed,
// and those that were made again at another size or in another format
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
        if path
            .extension()
            .is_some_and(|ext| ext == "png" || ext == "jpg")
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

// Makes the thumbnail of an album in `dir`, from the cover of its first
// track, and returns its path
fn create_thumbnail(track: &Path, dir: &Path, album_id: &str, size: u32) -> Option<PathBuf> {
    let track = cue_sheet::audio_path(track)?;
    let track = track.as_path();
    let (tagged_file, _) = read_file_tags(track, ParseOptions::new())?;
    // A file without tags can still have a cover file next to it
    let empty = Tag::new(tagged_file.primary_tag_type());
    let tag = file_tag(&tagged_file);
    let tag = tag.as_deref().unwrap_or(&empty);

    let image = CoverCache::load_cover(tag, track.parent(), size)?;
    let dest = dir.join(thumbnail_name(album_id, size, image.has_alpha()));
    let saved = if image.has_alpha() {
        image.save_png(&dest)
    } else {
        image.save_jpeg(&dest)
    };

    match saved {
        Ok(_) => Some(dest),
        Err(e) => {
            warn!("Unable to save album cover {dest:?}: {e}");
            None
        }
    }
}

// An audio file's tags, and the items lofty leaves out of them, which have
// the CUE sheet in the tags, without the covers and properties, which the
// library doesn't keep
fn read_tags_only(path: &Path) -> Option<(lofty::file::TaggedFile, ExtraComments)> {
    let mut options = ParseOptions::new();
    options.read_cover_art(false).read_properties(false);
    read_file_tags(path, options)
}

// The tracks of an audio file: the file, or the tracks that the CUE sheet
// in its tags splits it into
fn read_tags(path: &Path) -> Vec<(PathBuf, TrackTags)> {
    let Some((tagged_file, extra)) = read_tags_only(path) else {
        return vec![(path.to_path_buf(), TrackTags::default())];
    };
    let tag = file_tag(&tagged_file);

    let sheet = extra
        .cue_sheet
        .as_deref()
        .and_then(|text| SplitSheet::embedded(path, text));
    if let Some(sheet) = sheet {
        let album_tag = cue_sheet::album_tag(tag.as_deref(), &sheet.sheet);
        return sheet
            .tracks()
            .map(|(track, _)| cue_track_tags(&sheet, &album_tag, track))
            .collect();
    }

    // A file without tags is in the album of its folder too
    let empty = Tag::new(tagged_file.primary_tag_type());
    let tags = TrackTags::from_tag(tag.as_deref().unwrap_or(&empty), path, Some(path));
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
                let file = read_tags_only(audio);
                let tag = file.as_ref().and_then(|(file, _)| file_tag(file));
                cue_sheet::album_tag(tag.as_deref(), &sheet.sheet)
            });
            cue_track_tags(sheet, album_tag, track)
        })
        .collect()
}

fn cue_track_tags(sheet: &SplitSheet, album_tag: &Tag, track: &CueTrack) -> (PathBuf, TrackTags) {
    let tag = cue_sheet::track_tag(album_tag, &sheet.sheet, track);
    // The album is in the folder of the sheet
    let tags = TrackTags::from_tag(&tag, &sheet.path, None);
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

// The album of a track without an album tag: the folder it's in, by name
pub fn folder_album_title(path: &Path) -> Option<String> {
    album_folder(path)?
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

impl TrackTags {
    // The title of a track without one is the name of its `file`, which
    // is searched like the queue searches it; the tracks of CUE sheets
    // have none
    fn from_tag(tag: &Tag, path: &Path, file: Option<&Path>) -> Self {
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

        // The release id from MusicBrainz tells apart albums with the same
        // name; without it, we use the album artist, so the discs of an
        // album in separate folders stay together. Compilations often have
        // no album artist, and a different artist for each track, so we fall
        // back to the folder the album is in. Tracks without an album are
        // grouped by that folder, as an album without a name, which the
        // tags can't have
        let mbid = get(ItemKey::MusicBrainzReleaseId).filter(|s| album.is_some() && is_mbid(s));
        let album_id = mbid.unwrap_or_else(|| {
            let mut hasher = Sha256::new();
            hasher.update(album.as_deref().unwrap_or_default());
            hasher.update([0]);
            match (
                album.as_ref().and(album_artist.as_ref()),
                album_folder(path),
            ) {
                (Some(artist), _) => hasher.update(artist),
                (None, Some(folder)) => {
                    hasher.update(folder.as_os_str().as_encoded_bytes());
                }
                (None, None) => (),
            }
            utils::hex(&hasher.finalize())
        });
        // The album of a track without one is shown as its folder's name,
        // in the grid as in the queue
        let folder_album = album.is_none().then(|| folder_album_title(path)).flatten();

        // As `Track::with_search_key()` makes it, so that searching the albums
        // matches the same tracks as searching the queue
        let title = track_title(tag);
        let artists = tag.get_strings(ItemKey::TrackArtist).join(", ");
        let search = search_key(
            Some(&artists),
            title
                .clone()
                .or_else(|| file.map(title_from_file_name))
                .as_deref(),
            &tag_search_text(tag),
            folder_album.as_deref(),
        );

        TrackTags {
            album_id: Some(album_id),
            album: album.or(folder_album),
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
    use std::path::{Path, PathBuf};

    use super::{
        MusicFolder, Roots, is_mbid, path_bytes, path_from_bytes, thumbnail_name, thumbnail_size,
    };

    fn roots_of(paths: &[&Path]) -> Roots {
        let folders: Vec<MusicFolder> = paths
            .iter()
            .map(|path| MusicFolder::new(path.to_path_buf()))
            .collect();
        super::roots(&folders)
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dismas-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(super::SCHEMA).unwrap();
        conn
    }

    #[test]
    fn a_damaged_database_is_made_again() {
        let dir = test_dir("damaged");
        let path = dir.join("library.db");
        std::fs::write(&path, vec![0xa5; 8192]).unwrap();
        assert!(super::open_at(&path).is_err());

        let conn = super::open_or_recreate(&path).unwrap();
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tracks, 0);

        drop(conn);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // E.g. names from old Latin-1 rips
    #[cfg(unix)]
    #[test]
    fn paths_that_are_not_utf8_round_trip() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

        let path = Path::new(OsStr::from_bytes(b"/music/caf\xe9.flac"));
        assert_eq!(path_from_bytes(path_bytes(path).to_vec()), path);
    }

    // Like the queue, for which the title is the file name, and the album
    // the folder's
    #[test]
    fn tracks_without_titles_are_searched_by_file_name() {
        use lofty::tag::{ItemKey, Tag, TagType};

        let path = Path::new("/music/Some Folder/01 - Intro.flac");
        let mut tag = Tag::new(TagType::VorbisComments);
        let tags = super::TrackTags::from_tag(&tag, path, Some(path));
        assert_eq!(tags.album.as_deref(), Some("Some Folder"));
        let search = tags.search.unwrap();
        assert!(search.contains("intro"));
        assert!(search.contains("some folder"));

        tag.insert_text(ItemKey::AlbumTitle, "Album".to_string());
        let tags = super::TrackTags::from_tag(&tag, path, Some(path));
        assert_eq!(tags.title, None);
        let search = tags.search.unwrap();
        assert!(search.contains("intro"));
        assert!(!search.contains("some folder"));

        tag.insert_text(ItemKey::TrackTitle, "Overture".to_string());
        let tags = super::TrackTags::from_tag(&tag, path, Some(path));
        let search = tags.search.unwrap();
        assert!(search.contains("overture"));
        assert!(!search.contains("intro"));
    }

    #[test]
    fn tracks_without_an_album_are_grouped_by_folder() {
        use super::{query_albums, scan_with};
        use crate::utils::write_test_wav;

        let music = test_dir("folder-albums");
        std::fs::create_dir_all(music.join("Album/CD 2")).unwrap();
        std::fs::create_dir_all(music.join("Sheet")).unwrap();
        write_test_wav(&music.join("Loose.wav"), 1);
        write_test_wav(&music.join("Album/One.wav"), 1);
        write_test_wav(&music.join("Album/CD 2/Two.wav"), 1);
        write_test_wav(&music.join("Sheet/Sheet.wav"), 4);
        std::fs::write(
            music.join("Sheet/Sheet.cue"),
            "FILE \"Sheet.wav\" WAVE\n\
             TRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 00:02:00\n",
        )
        .unwrap();

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        scan_with(&mut conn, &roots_of(&[&music]), &sender).unwrap();

        // The discs of an album in folders of their own are one album, and
        // the tracks in the music folder itself are named after it
        let root = music.file_name().unwrap().to_string_lossy().into_owned();
        let mut albums: Vec<(String, String)> = query_albums(&conn, &[])
            .unwrap()
            .into_iter()
            .map(|album| (album.title, album.artist))
            .collect();
        albums.sort();
        let mut expected: Vec<(String, String)> = ["Album", "Sheet", &root]
            .iter()
            .map(|title| (title.to_string(), "Unknown artist".to_string()))
            .collect();
        expected.sort();
        assert_eq!(albums, expected);

        let mut stmt = conn
            .prepare("SELECT path, album_id, search FROM tracks ORDER BY path")
            .unwrap();
        let rows: Vec<(Vec<u8>, String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let album_of = |name: &str| {
            let path = path_bytes(&music.join(name)).to_vec();
            rows.iter().find(|row| row.0 == path).unwrap().1.clone()
        };
        assert_eq!(album_of("Album/One.wav"), album_of("Album/CD 2/Two.wav"));
        assert_eq!(
            album_of("Sheet/Sheet.cue/track01"),
            album_of("Sheet/Sheet.cue/track02")
        );
        assert_ne!(album_of("Album/One.wav"), album_of("Loose.wav"));

        // The folder is searched like a tag of the album
        let two = path_bytes(&music.join("Album/CD 2/Two.wav")).to_vec();
        let search = &rows.iter().find(|row| row.0 == two).unwrap().2;
        assert!(search.contains("album"));
        assert!(search.contains("two"));

        std::fs::remove_dir_all(&music).unwrap();
    }

    #[test]
    fn searching_counts_the_matching_tracks_of_each_album() {
        let conn = test_db();
        let tracks: [(&str, &str, Option<&str>); 6] = [
            ("/music/mass/1.opus", "mass", Some("kyrie soprano gardiner")),
            ("/music/mass/2.opus", "mass", Some("gloria gardiner")),
            ("/music/mass/3.opus", "mass", None),
            (
                "/music/passion/1.opus",
                "passion",
                Some("aria soprano gardiner"),
            ),
            ("/music/unmounted/1.opus", "passion", Some("aria gardiner")),
            ("/music/songs/1.opus", "songs", Some("lied tenor")),
        ];
        for (path, album_id, search) in tracks {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
                rusqlite::params![path.as_bytes(), album_id, search],
            )
            .unwrap();
        }
        let search = |words: &[&str]| {
            let words: Vec<String> = words.iter().map(ToString::to_string).collect();
            let hidden = [PathBuf::from("/music/unmounted")];
            let mut albums: Vec<(String, (u32, u32))> =
                super::query_search_albums(&conn, &words, &hidden)
                    .unwrap()
                    .into_iter()
                    .collect();
            albums.sort();
            albums
        };

        // The track without search text counts towards its album only, and
        // the one in a folder that isn't there doesn't count at all
        assert_eq!(
            search(&["gardiner"]),
            [
                ("mass".to_string(), (2, 3)),
                ("passion".to_string(), (1, 1))
            ]
        );
        assert_eq!(
            search(&["soprano", "gardiner"]),
            [
                ("mass".to_string(), (1, 3)),
                ("passion".to_string(), (1, 1))
            ]
        );
        assert_eq!(search(&["tenor"]), [("songs".to_string(), (1, 1))]);
        assert!(search(&["bass"]).is_empty());
    }

    #[test]
    fn only_uuids_are_musicbrainz_ids() {
        assert!(is_mbid("9e8f8d6e-3a1c-4b4f-9b0c-2f1d6a5e7c3b"));
        assert!(!is_mbid(""));
        assert!(!is_mbid("../../Pictures/photo"));
        assert!(!is_mbid("/home/user/Pictures/photo"));
    }

    // The grid's covers were PNG files of 480 pixels; made for another
    // size, or in the old format, they're made again
    #[test]
    fn thumbnails_are_named_by_size_and_format() {
        let dir = Path::new("/cache/album-covers");
        assert_eq!(thumbnail_name("album", 240, false), "album-240.jpg");
        assert_eq!(thumbnail_name("album", 480, true), "album-480-alpha.png");

        let size = |name: &str| thumbnail_size(&dir.join(name), dir, "album");
        assert_eq!(size("album-240.jpg"), Some(240));
        assert_eq!(size("album-480-alpha.png"), Some(480));
        // The PNG files of older versions
        assert_eq!(size("album-480.png"), None);
        assert_eq!(size("other-240.jpg"), None);
        assert_eq!(size("album-other-240.jpg"), None);
        assert_eq!(size("album-.jpg"), None);
        assert_eq!(
            thumbnail_size(Path::new("/elsewhere/album-240.jpg"), dir, "album"),
            None
        );
    }

    // A track in the database, with paths relative to a folder
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
        use super::scan_with;
        use crate::utils::write_test_wav;

        let music = test_dir("cue");
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

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();

        let (result, _) = scan_with(&mut conn, &roots_of(&[&music]), &sender).unwrap();
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
        let (result, _) = scan_with(&mut conn, &roots_of(&[&music]), &sender).unwrap();
        assert_eq!(result.n_changed, 0);

        // Without the sheet, the file is a track again, added when its
        // tracks were
        std::fs::remove_file(&cue).unwrap();
        let (result, _) = scan_with(&mut conn, &roots_of(&[&music]), &sender).unwrap();
        assert_eq!(result.n_changed, 3);
        let found = tracks(&conn, &music);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "Album/Album.wav");
        assert_eq!(found[0].file, "Album/Album.wav");
        assert_eq!(found[0].added, added);

        std::fs::remove_dir_all(&music).unwrap();
    }

    #[test]
    fn folders_inside_others_are_left_out() {
        use super::outermost;

        let keyed = |keys: &[&str]| -> Vec<(PathBuf, usize)> {
            keys.iter()
                .enumerate()
                .map(|(i, key)| (PathBuf::from(key), i))
                .collect()
        };
        assert_eq!(outermost(keyed(&["/m/a/sub", "/m/a"])), vec![1]);
        assert_eq!(outermost(keyed(&["/m/a", "/m/a/sub"])), vec![0]);
        assert_eq!(outermost(keyed(&["/m/a", "/m/b", "/m/a"])), vec![0, 1]);
        // Not a folder of /m/a
        assert_eq!(outermost(keyed(&["/m/a", "/m/ab"])), vec![0, 1]);
    }

    #[test]
    fn missing_and_empty_folders_are_unavailable() {
        let dir = test_dir("roots");
        let (full, empty, missing) = (dir.join("full"), dir.join("empty"), dir.join("missing"));
        std::fs::create_dir_all(&full).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(full.join("file"), "").unwrap();

        let roots = roots_of(&[&full, &empty, &missing]);
        assert_eq!(roots.available, vec![full]);
        assert_eq!(roots.unavailable, vec![empty, missing]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_settings_list_the_folders() {
        use gtk::gio::prelude::*;

        use super::{default_music_dir, folders_from_entries};

        assert_eq!(
            folders_from_entries(Vec::new(), Vec::new()),
            vec![MusicFolder::new(default_music_dir())]
        );
        assert_eq!(
            folders_from_entries(
                vec![
                    ("file:///music/A".to_string(), String::new()),
                    (
                        "file:///run/user/1000/doc/1a2b/B".to_string(),
                        "file:///media/drive/B".to_string()
                    ),
                ],
                vec![
                    "file:///run/user/1000/doc/1a2b/B/Podcasts/Old".to_string(),
                    "file:///music/A/Audiobooks".to_string(),
                    "file:///music/A/Audiobooks".to_string(),
                    // Neither in a folder, nor one of its folders
                    "file:///music/Other".to_string(),
                    "file:///music/A".to_string(),
                ]
            ),
            vec![
                MusicFolder {
                    path: PathBuf::from("/music/A"),
                    host: None,
                    excluded: vec![PathBuf::from("Audiobooks")],
                },
                MusicFolder {
                    path: PathBuf::from("/run/user/1000/doc/1a2b/B"),
                    host: Some(PathBuf::from("/media/drive/B")),
                    excluded: vec![PathBuf::from("Podcasts/Old")],
                },
            ]
        );

        // Those of the XDG Music folder too
        let audiobooks = default_music_dir().join("Audiobooks");
        let uri = gtk::gio::File::for_path(&audiobooks).uri().to_string();
        assert_eq!(
            folders_from_entries(Vec::new(), vec![uri])[0].excluded,
            vec![PathBuf::from("Audiobooks")]
        );
    }

    #[test]
    fn left_out_folders_are_not_read() {
        use super::scan_with;
        use crate::utils::write_test_wav;

        let dir = test_dir("excluded");
        for folder in ["Album", "Audiobooks/Book", "Podcasts/New", "Podcasts/Old"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        write_test_wav(&dir.join("Album/One.wav"), 1);
        write_test_wav(&dir.join("Audiobooks/Book/Two.wav"), 1);
        write_test_wav(&dir.join("Podcasts/New/Three.wav"), 1);
        write_test_wav(&dir.join("Podcasts/Old/Four.wav"), 1);

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        let paths = |conn: &rusqlite::Connection| -> Vec<String> {
            tracks(conn, &dir).into_iter().map(|row| row.path).collect()
        };
        let folder = |excluded: &[&str]| MusicFolder {
            excluded: excluded.iter().map(PathBuf::from).collect(),
            ..MusicFolder::new(dir.clone())
        };

        let roots = super::roots(&[folder(&["Podcasts/Old"])]);
        assert_eq!(roots.excluded, vec![dir.join("Podcasts/Old")]);
        scan_with(&mut conn, &roots, &sender).unwrap();
        assert_eq!(
            paths(&conn),
            vec![
                "Album/One.wav",
                "Audiobooks/Book/Two.wav",
                "Podcasts/New/Three.wav"
            ]
        );

        // The tracks read before a folder was left out are forgotten
        let roots = super::roots(&[folder(&["Audiobooks", "Podcasts"])]);
        let (result, _) = scan_with(&mut conn, &roots, &sender).unwrap();
        assert_eq!(result.n_changed, 2);
        assert_eq!(paths(&conn), vec!["Album/One.wav"]);

        // And read again once it's back
        let (result, _) = scan_with(&mut conn, &super::roots(&[folder(&[])]), &sender).unwrap();
        assert_eq!(result.n_changed, 3);

        // Those of a folder inside another one don't count, as the other
        // one is read
        let inner = MusicFolder {
            excluded: vec![PathBuf::from("Old")],
            ..MusicFolder::new(dir.join("Podcasts"))
        };
        let roots = super::roots(&[folder(&[]), inner]);
        assert!(roots.excluded.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn left_out_folders_are_relative_to_their_music_folder() {
        use super::relative_to;

        let dir = test_dir("relative");
        let music = dir.join("Music");
        std::fs::create_dir_all(music.join("Audiobooks")).unwrap();
        let link = dir.join("Link");
        std::os::unix::fs::symlink(&music, &link).unwrap();

        let folder = |path: PathBuf| MusicFolder::new(path);
        assert_eq!(
            relative_to(&folder(music.join("Audiobooks")), &folder(music.clone())),
            Some(PathBuf::from("Audiobooks"))
        );
        // Picked through a link to the music folder
        assert_eq!(
            relative_to(&folder(link.join("Audiobooks")), &folder(music.clone())),
            Some(PathBuf::from("Audiobooks"))
        );
        assert_eq!(
            relative_to(&folder(music.clone()), &folder(music.clone())),
            None
        );
        assert_eq!(
            relative_to(&folder(dir.clone()), &folder(music.clone())),
            None
        );
        // Through the document portal, by the paths outside the sandbox
        let portal = |path: &str, host: &str| MusicFolder {
            host: Some(PathBuf::from(host)),
            ..MusicFolder::new(PathBuf::from(path))
        };
        assert_eq!(
            relative_to(
                &portal("/run/user/1000/doc/9z/Old", "/media/drive/B/Podcasts/Old"),
                &portal("/run/user/1000/doc/1a2b/B", "/media/drive/B")
            ),
            Some(PathBuf::from("Podcasts/Old"))
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tracks_of_several_folders() {
        use super::scan_with;
        use crate::utils::write_test_wav;

        let dir = test_dir("folders");
        let (a, b) = (dir.join("A"), dir.join("B"));
        let sub = a.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        write_test_wav(&a.join("One.wav"), 1);
        write_test_wav(&sub.join("Two.wav"), 1);
        write_test_wav(&b.join("Three.wav"), 1);

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        let paths = |conn: &rusqlite::Connection| -> Vec<String> {
            tracks(conn, &dir).into_iter().map(|row| row.path).collect()
        };

        let (result, _) = scan_with(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
        assert_eq!(result.n_changed, 3);
        assert_eq!(
            paths(&conn),
            vec!["A/One.wav", "A/sub/Two.wav", "B/Three.wav"]
        );

        // A folder inside another one is read once, either way round
        for roots in [roots_of(&[&a, &sub, &b]), roots_of(&[&sub, &a, &b])] {
            let (result, _) = scan_with(&mut conn, &roots, &sender).unwrap();
            assert_eq!(result.n_changed, 0);
            assert_eq!(paths(&conn).len(), 3);
        }

        // The tracks of a folder that isn't there are kept, as are those of
        // an empty one, which is often a drive that isn't mounted
        let away = dir.join("B-away");
        std::fs::rename(&b, &away).unwrap();
        let (result, _) = scan_with(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
        assert_eq!(result.n_changed, 0);
        assert_eq!(paths(&conn).len(), 3);
        std::fs::rename(&away, &b).unwrap();
        std::fs::rename(b.join("Three.wav"), dir.join("Three.wav")).unwrap();
        let (result, _) = scan_with(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
        assert_eq!(result.n_changed, 0);
        assert_eq!(paths(&conn).len(), 3);

        // Those of a folder that was removed from the list go
        let (result, _) = scan_with(&mut conn, &roots_of(&[&a]), &sender).unwrap();
        assert_eq!(result.n_changed, 1);
        assert_eq!(paths(&conn), vec!["A/One.wav", "A/sub/Two.wav"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_linked_to_another_is_read_once() {
        use super::scan_with;
        use crate::utils::write_test_wav;

        let dir = test_dir("link");
        let a = dir.join("A");
        std::fs::create_dir_all(&a).unwrap();
        write_test_wav(&a.join("One.wav"), 1);
        let b = dir.join("B");
        std::os::unix::fs::symlink(&a, &b).unwrap();

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        let (result, _) = scan_with(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
        assert_eq!(result.n_changed, 1);
        let found: Vec<String> = tracks(&conn, &dir)
            .into_iter()
            .map(|row| row.path)
            .collect();
        assert_eq!(found, vec!["A/One.wav"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trash_and_hidden_folders_are_not_read() {
        use super::scan_with;
        use crate::utils::write_test_wav;

        let dir = test_dir("hidden");
        for folder in ["Album", ".Trash-1000/files", "$RECYCLE.BIN"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        write_test_wav(&dir.join("Album/One.wav"), 1);
        write_test_wav(&dir.join("Album/.Two.wav"), 1);
        write_test_wav(&dir.join(".Trash-1000/files/Three.wav"), 1);
        write_test_wav(&dir.join("$RECYCLE.BIN/$R4ND0M.wav"), 1);

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        scan_with(&mut conn, &roots_of(&[&dir]), &sender).unwrap();
        let found: Vec<String> = tracks(&conn, &dir)
            .into_iter()
            .map(|row| row.path)
            .collect();
        assert_eq!(found, vec!["Album/One.wav"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_albums_of_hidden_folders_are_left_out() {
        use super::query_albums;

        let conn = test_db();
        for (path, album) in [
            ("/m/A/One.flac", "a"),
            ("/m/B/Two.flac", "b"),
            ("/m/B/sub/Three.flac", "b"),
            ("/m/Bb/Four.flac", "c"),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2)",
                rusqlite::params![path.as_bytes(), album],
            )
            .unwrap();
        }
        let ids = |hidden: &[PathBuf]| -> Vec<String> {
            let mut ids: Vec<String> = query_albums(&conn, hidden)
                .unwrap()
                .into_iter()
                .map(|album| album.id)
                .collect();
            ids.sort();
            ids
        };

        assert_eq!(ids(&[]), vec!["a", "b", "c"]);
        assert_eq!(ids(&[PathBuf::from("/m/B")]), vec!["a", "c"]);
        assert_eq!(
            ids(&[PathBuf::from("/m/A"), PathBuf::from("/m/Bb/")]),
            vec!["b"]
        );
    }
}
