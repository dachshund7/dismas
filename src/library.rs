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
mod watch;

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
    prelude::Accessor,
    tag::{ItemKey, Tag},
};
use log::{debug, warn};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
pub use watch::FolderWatch;

use crate::{
    audio::{
        CoverCache, ExtraComments, FileTags, ListedPerformer, PerformerKind, ReadOptions,
        TrackCredits, normalize_search_text, plain_commas, read_file_tags, search_key,
        split_composers, title_from_file_name, track_search_text, track_title,
    },
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::i18n,
    utils,
};

// Bump when the schema or the way the tags are read changes, so that
// the whole library is read again
const SCHEMA_VERSION: i32 = 22;

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
        search TEXT,
        composers TEXT,
        work_key TEXT,
        work TEXT,
        work_title TEXT,
        work_collection TEXT,
        work_composers TEXT,
        movement_order TEXT,
        performers TEXT,
        label TEXT,
        catalog_number TEXT
    );
    CREATE INDEX tracks_album ON tracks (album_id);
    CREATE INDEX tracks_file ON tracks (file);
    CREATE INDEX tracks_work ON tracks (work_key);
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
    // What to watch for changes, see `folders_to_watch()`
    pub watch: WatchList,
}

// What to watch for changes in the music folders, as of a scan
#[derive(Debug, Default)]
pub struct WatchList {
    // The folders to watch for files that come, go or change
    pub folders: Vec<PathBuf>,
    // Whether a music folder that's there can't be watched, so that it's
    // read again when the albums are shown
    pub unwatched: bool,
    // The music folders that are there, and the folders that symbolic
    // links in them lead to, without symbolic links: a drive unmounted can
    // take one away, and a drive mounted in one adds to it. While one
    // isn't there, or a link leads nowhere, a drive mounted anywhere can
    // bring it back, as the folder a link leads to is only known once
    // it's there
    pub available: Vec<PathBuf>,
    pub unavailable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumInfo {
    pub id: String,
    pub title: String,
    pub artist: String,
    // The key the artist is sorted by
    pub artist_sort: String,
    // The year its tracks were made, the earliest, see `TrackCredits::year()`
    pub year: Option<String>,
    // When the first track of the album was added, in seconds since the
    // epoch
    pub added: Option<i64>,
    pub cover: Option<String>,
    // The record label, and the number it gives the release
    pub label: Option<String>,
    pub catalog_number: Option<String>,
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
    // The composers, see `composer_entries()`
    composers: Option<String>,
    // The work the track performs, or a movement of, see `WorkEntry`
    work: Option<WorkEntry>,
    // The conductors and performers, see `performer_entries()`
    performers: Option<String>,
    // The first record label, and the first catalogue number
    label: Option<String>,
    catalog_number: Option<String>,
}

// The work of a track, as the library keeps it
#[derive(Debug, PartialEq)]
struct WorkEntry {
    // What tells the work apart from others across albums: its MusicBrainz
    // id, or else its name and its composers, see `work_entry()`
    key: String,
    // The name the queue shows it by
    name: String,
    // The full title, when it's not the name
    title: Option<String>,
    // The work it's part of, or the collection it's in
    collection: Option<String>,
    // Its composers, as in `composer_entries()`
    composers: Option<String>,
    // Where the movement is in the work, e.g. "1.3.3", or "" if that isn't
    // known; `None` if the track performs the whole work
    order: Option<String>,
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

// The paths of the music folders, to look for tracks in
pub fn music_folder_paths() -> Vec<PathBuf> {
    music_folders(&utils::settings_manager())
        .into_iter()
        .map(|folder| folder.path)
        .collect()
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
// a drive, which keeps deleted files under their own names, and neither
// are the recycle bins of Windows and of network drives, nor their
// snapshots, which keep copies of the files, nor Synology's thumbnails
pub fn is_library_entry(path: &Path) -> bool {
    const LEFT_OUT: [&[u8]; 6] = [
        b"$recycle.bin",
        b"recycler",
        b"#recycle",
        b"#snapshot",
        b"@recycle",
        b"@eadir",
    ];
    let Some(name) = path.file_name() else {
        return true;
    };
    let name = name.as_encoded_bytes();
    !name.starts_with(b".")
        && !LEFT_OUT
            .iter()
            .any(|left_out| name.eq_ignore_ascii_case(left_out))
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
                COUNT(DISTINCT t.artist), MAX(t.label), MAX(t.catalog_number)
         FROM tracks t LEFT JOIN covers c ON c.album_id = t.album_id
         WHERE t.album_id IS NOT NULL{hidden_sql}
         GROUP BY t.album_id"
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
        let info = AlbumInfo {
            id: row.get(0)?,
            title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            artist,
            artist_sort: sort_key,
            year: row.get(3)?,
            added: row.get(6)?,
            cover: row.get(4)?,
            label: row.get(8)?,
            catalog_number: row.get(9)?,
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

// What the tracks taken from albums match: every word of a search, normalized
// with `normalize_search_text()`, in any of their tags, a composer and a
// performer, by their keys (`composer_key()`). An empty one matches every
// track
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackFilter {
    pub words: Vec<String>,
    pub composer: Option<String>,
    pub performer: Option<String>,
}

impl TrackFilter {
    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.composer.is_none() && self.performer.is_none()
    }

    // The SQL conditions on a track, each after " AND ", and their values;
    // a track without search text, composers or performers matches nothing
    fn conditions(&self) -> (String, Vec<String>) {
        let mut sql = String::new();
        let mut values = Vec::new();
        for word in &self.words {
            sql.push_str(" AND instr(search, ?) > 0");
            values.push(word.clone());
        }
        if let Some(key) = &self.composer {
            sql.push_str(" AND instr(composers, ?) > 0");
            values.push(format!("\n{key}\t"));
        }
        if let Some(key) = &self.performer {
            sql.push_str(" AND instr(performers, ?) > 0");
            values.push(format!("\t{key}\t"));
        }
        (sql, values)
    }

    // An SQL expression of whether a track matches, 1 or 0, for the values
    // of `conditions()`; `instr()` on a track without composers or
    // performers gives NULL, which only a WHERE takes as false
    fn matches_sql(&self) -> String {
        format!("(CASE WHEN 1{} THEN 1 ELSE 0 END)", self.conditions().0)
    }
}

// The values of a statement that has the filter's `matches_sql()` before
// the condition on the `hidden` folders, see `hidden_condition()`
fn filter_params(
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::ParamsFromIter<Vec<rusqlite::types::Value>> {
    use rusqlite::types::Value;

    let (_, values) = filter.conditions();
    let (_, hidden_params) = hidden_condition("file", hidden);
    rusqlite::params_from_iter(
        values
            .into_iter()
            .map(Value::Text)
            .chain(hidden_params.into_iter().map(Value::Blob))
            .collect(),
    )
}

// The albums with a track that the filter matches, like searching the
// queue, with the number of their tracks that it does and the number of all
// their tracks; the tracks in the `hidden` folders are left out, as in
// `albums()`
pub fn search_albums(filter: &TrackFilter, hidden: &[PathBuf]) -> HashMap<String, (u32, u32)> {
    match with_connection(|conn| query_search_albums(conn, filter, hidden)) {
        Ok(albums) => albums,
        Err(e) => {
            warn!("Unable to search the albums: {e}");
            HashMap::new()
        }
    }
}

fn query_search_albums(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<HashMap<String, (u32, u32)>> {
    use rusqlite::types::Value;

    let (conditions, values) = filter.conditions();
    let conditions = format!("1{conditions}");
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, SUM(CASE WHEN {conditions} THEN 1 ELSE 0 END) AS matching,
                COUNT(*)
         FROM tracks
         WHERE album_id IS NOT NULL{hidden_sql}
         GROUP BY album_id
         HAVING matching > 0"
    ))?;
    let params = values
        .into_iter()
        .map(Value::Text)
        .chain(hidden_params.into_iter().map(Value::Blob));
    stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
    })?
    .collect()
}

// The albums of the grid, by their ids, with the paths of their tracks in
// their order, without the tracks in the hidden folders
pub fn album_track_paths(hidden: &[PathBuf]) -> Vec<(String, Vec<PathBuf>)> {
    match with_connection(|conn| query_album_track_paths(conn, hidden)) {
        Ok(albums) => albums,
        Err(e) => {
            warn!("Unable to load the tracks of the albums: {e}");
            Vec::new()
        }
    }
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
struct TrackOrder {
    disc: i64,
    folder: Option<(bool, glib::FilenameCollationKey)>,
    track: i64,
    name: (bool, glib::FilenameCollationKey),
}

impl TrackOrder {
    fn new(path: &Path, disc: Option<i64>, track: Option<i64>) -> Self {
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

// A composer of the tracks of the albums of the grid
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerInfo {
    // See `composer_key()`
    pub key: String,
    // The name its tracks give it the most, and the name it's sorted by
    pub name: String,
    pub sort: String,
    // What the list shows: the sort name, as in "Bach, Johann Sebastian",
    // when the tags give one, and otherwise the name, as the sort name
    // worked out from it would turn a band's name around, or a name whose
    // family name is first already
    pub label: String,
    // Both names, normalized with `normalize_search_text()`, which
    // searching the list looks into
    pub search: String,
    // The number of albums with a track by the composer
    pub albums: u32,
}

// Whether a track of the albums of the grid has a composer, without the
// tracks in the `hidden` folders
pub fn has_composers(hidden: &[PathBuf]) -> bool {
    match with_connection(|conn| query_has_composers(conn, hidden)) {
        Ok(has) => has,
        Err(e) => {
            warn!("Unable to look for composers: {e}");
            false
        }
    }
}

fn query_has_composers(conn: &Connection, hidden: &[PathBuf]) -> rusqlite::Result<bool> {
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM tracks
             WHERE album_id IS NOT NULL AND composers IS NOT NULL{hidden_sql})"
        ),
        rusqlite::params_from_iter(hidden_params),
        |row| row.get(0),
    )
}

// The composers of the tracks of the albums of the grid that the filter
// matches, by their sort names, without the tracks in the `hidden` folders,
// with the number of the albums of those tracks
pub fn composers(hidden: &[PathBuf], filter: &TrackFilter) -> Vec<ComposerInfo> {
    match with_connection(|conn| query_composers(conn, hidden, filter)) {
        Ok(composers) => composers,
        Err(e) => {
            warn!("Unable to list the composers: {e}");
            Vec::new()
        }
    }
}

// A composer's names are the ones all their tracks give them the most,
// whatever the filter, so that they're the same in every list
fn query_composers(
    conn: &Connection,
    hidden: &[PathBuf],
    filter: &TrackFilter,
) -> rusqlite::Result<Vec<ComposerInfo>> {
    // The albums of a composer that the filter matches, and how often each
    // of its names is given
    #[derive(Default)]
    struct Seen {
        albums: HashSet<String>,
        names: HashMap<String, usize>,
        sorts: HashMap<String, usize>,
    }

    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, composers, {} FROM tracks
         WHERE album_id IS NOT NULL AND composers IS NOT NULL{}",
        filter.matches_sql(),
        hidden_condition("file", hidden).0
    ))?;
    let rows = stmt.query_map(filter_params(filter, hidden), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })?;
    let mut seen: HashMap<String, Seen> = HashMap::new();
    for row in rows {
        let (album, entries, matches) = row?;
        for line in entries.lines() {
            let mut fields = line.split('\t');
            let (Some(key), Some(name), Some(sort)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let composer = seen.entry(key.to_string()).or_default();
            if matches && !composer.albums.contains(&album) {
                composer.albums.insert(album.clone());
            }
            *composer.names.entry(name.to_string()).or_default() += 1;
            if !sort.is_empty() {
                *composer.sorts.entry(sort.to_string()).or_default() += 1;
            }
        }
    }

    let mut composers: Vec<(glib::CollationKey, ComposerInfo)> = seen
        .into_iter()
        .filter(|(_, seen)| !seen.albums.is_empty())
        .map(|(key, seen)| {
            let name = most_common(seen.names);
            let tagged = most_common(seen.sorts);
            let sort = composer_sort(&name, &tagged);
            let label = if tagged.is_empty() {
                name.clone()
            } else {
                tagged
            };
            let info = ComposerInfo {
                key,
                search: normalize_search_text(&format!("{name} {sort}")),
                name,
                sort: sort.clone(),
                label,
                albums: seen.albums.len() as u32,
            };
            (glib::CollationKey::from(sort.as_str()), info)
        })
        .collect();
    composers.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.key.cmp(&b.1.key)));
    Ok(composers.into_iter().map(|(_, info)| info).collect())
}

// Of the values counted, the most common, and of those the first in
// alphabetical order
fn most_common(counts: HashMap<String, usize>) -> String {
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(value, _)| value)
        .unwrap_or_default()
}

// A conductor or performer of the tracks of the albums of the grid, see
// `performer_entries()`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerformerInfo {
    // See `composer_key()`
    pub key: String,
    // Whether they conduct, are a group or play, by most of their albums,
    // see `query_performers()`
    pub kind: PerformerKind,
    // The name their tracks give them the most, and the name they're
    // sorted by
    pub name: String,
    pub sort: String,
    // What they play or sing, the most common first, e.g. "piano" and
    // "harpsichord"
    pub roles: Vec<String>,
    // The names and the roles, normalized with `normalize_search_text()`,
    // which searching the list looks into, so that "piano" finds the
    // pianists
    pub search: String,
    // The number of albums with a track they perform on
    pub albums: u32,
}

// Whether a track of the albums of the grid has a conductor or performer,
// without the tracks in the `hidden` folders
pub fn has_performers(hidden: &[PathBuf]) -> bool {
    match with_connection(|conn| query_has_performers(conn, hidden)) {
        Ok(has) => has,
        Err(e) => {
            warn!("Unable to look for performers: {e}");
            false
        }
    }
}

fn query_has_performers(conn: &Connection, hidden: &[PathBuf]) -> rusqlite::Result<bool> {
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM tracks
             WHERE album_id IS NOT NULL AND performers IS NOT NULL{hidden_sql})"
        ),
        rusqlite::params_from_iter(hidden_params),
        |row| row.get(0),
    )
}

// The conductors, groups and players of the tracks of the albums of the
// grid that the filter matches, in that order, each by their sort names,
// without the tracks in the `hidden` folders, with the number of the
// albums of those tracks
pub fn performers(hidden: &[PathBuf], filter: &TrackFilter) -> Vec<PerformerInfo> {
    match with_connection(|conn| query_performers(conn, hidden, filter)) {
        Ok(performers) => performers,
        Err(e) => {
            warn!("Unable to list the performers: {e}");
            Vec::new()
        }
    }
}

// Each performer is listed once, as what most of their albums have them
// do, as someone who conducts can play too. A name in the album artist
// that a track doesn't credit is taken to be a group's, without a role,
// though it can be a player's that other tracks credit, so it only counts
// when nothing else does. What they do, their names and their roles go by
// all their tracks, whatever the filter, so that they're under the same
// header, and named the same, in every list
fn query_performers(
    conn: &Connection,
    hidden: &[PathBuf],
    filter: &TrackFilter,
) -> rusqlite::Result<Vec<PerformerInfo>> {
    // The albums of a performer that the filter matches, the albums they
    // have each kind on, and how often each of their names and roles is
    // given
    #[derive(Default)]
    struct Seen {
        albums: HashSet<String>,
        kinds: HashMap<PerformerKind, HashSet<String>>,
        names: HashMap<String, usize>,
        sorts: HashMap<String, usize>,
        roles: HashMap<String, usize>,
    }

    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, performers, {} FROM tracks
         WHERE album_id IS NOT NULL AND performers IS NOT NULL{}",
        filter.matches_sql(),
        hidden_condition("file", hidden).0
    ))?;
    let rows = stmt.query_map(filter_params(filter, hidden), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })?;
    let mut seen: HashMap<String, Seen> = HashMap::new();
    for row in rows {
        let (album, entries, matches) = row?;
        for line in entries.lines() {
            let mut fields = line.split('\t');
            let (Some(kind), Some(key), Some(name), Some(sort), Some(role)) = (
                fields.next().and_then(performer_kind_from_code),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            let performer = seen.entry(key.to_string()).or_default();
            if matches && !performer.albums.contains(&album) {
                performer.albums.insert(album.clone());
            }
            let guessed = kind == PerformerKind::Ensemble && role.is_empty();
            if !guessed {
                performer
                    .kinds
                    .entry(kind)
                    .or_default()
                    .insert(album.clone());
            }
            *performer.names.entry(name.to_string()).or_default() += 1;
            *performer.sorts.entry(sort.to_string()).or_default() += 1;
            if !role.is_empty() {
                *performer.roles.entry(role.to_string()).or_default() += 1;
            }
        }
    }

    let mut performers: Vec<(glib::CollationKey, PerformerInfo)> = seen
        .into_iter()
        .filter(|(_, seen)| !seen.albums.is_empty())
        .map(|(key, seen)| {
            // With as many albums, conducting comes first, and a group last
            let preference = |kind: &PerformerKind| match kind {
                PerformerKind::Conductor => 2,
                PerformerKind::Soloist => 1,
                PerformerKind::Ensemble => 0,
            };
            let kind = seen
                .kinds
                .iter()
                .max_by(|a, b| {
                    a.1.len()
                        .cmp(&b.1.len())
                        .then_with(|| preference(a.0).cmp(&preference(b.0)))
                })
                .map_or(PerformerKind::Ensemble, |(kind, _)| *kind);
            let name = most_common(seen.names);
            let sort = match kind {
                PerformerKind::Ensemble => name.clone(),
                PerformerKind::Conductor | PerformerKind::Soloist => most_common(seen.sorts),
            };
            let roles: Vec<String> = seen
                .roles
                .into_iter()
                .sorted_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)))
                .map(|(role, _)| role)
                .collect();
            let info = PerformerInfo {
                key,
                kind,
                search: normalize_search_text(&format!("{name} {sort} {}", roles.join(" "))),
                name,
                sort: sort.clone(),
                roles,
                albums: seen.albums.len() as u32,
            };
            (glib::CollationKey::from(sort.as_str()), info)
        })
        .collect();
    performers.sort_by(|a, b| {
        a.1.kind
            .cmp(&b.1.kind)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.key.cmp(&b.1.key))
    });
    Ok(performers.into_iter().map(|(_, info)| info).collect())
}

// A work of the albums of the grid, see `WorkEntry`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkInfo {
    pub key: String,
    // The name its tracks give it the most, and its full title, if that's
    // not the name
    pub name: String,
    pub title: Option<String>,
    // The work it's part of, or the collection it's in
    pub collection: Option<String>,
    pub composers: Vec<WorkComposer>,
    // The number of its recordings that the filter matches, see
    // `recordings_of_works()`
    pub recordings: u32,
}

// A composer of a work, see `composer_entries()`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkComposer {
    pub key: String,
    pub name: String,
    pub sort: String,
}

// A recording of a work: its tracks on an album, see
// `recordings_of_works()`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRecording {
    pub album_id: String,
    pub album: Option<String>,
    pub year: Option<String>,
    // The conductors and performers of its tracks, as the credits line of
    // a track names them, see `performers_line()`
    pub performers: String,
    pub paths: Vec<PathBuf>,
}

// A track of an album with works, as the recordings of the works are
// worked out from
struct WorkRow {
    album_id: String,
    path: PathBuf,
    work: Option<String>,
    // See `WorkEntry::order`, with "" as an empty list
    order: Option<Vec<u32>>,
    // Whether the filter matches it
    matching: bool,
    name: Option<String>,
    title: Option<String>,
    collection: Option<String>,
    composers: Option<String>,
    album: Option<String>,
    year: Option<String>,
    performers: Option<String>,
}

// Whether a track of the albums of the grid is part of a work, without the
// tracks in the `hidden` folders
pub fn has_works(hidden: &[PathBuf]) -> bool {
    let query = |conn: &Connection| -> rusqlite::Result<bool> {
        let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
        conn.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM tracks
                 WHERE album_id IS NOT NULL AND work_key IS NOT NULL{hidden_sql})"
            ),
            rusqlite::params_from_iter(hidden_params),
            |row| row.get(0),
        )
    };
    match with_connection(|conn| query(conn)) {
        Ok(has) => has,
        Err(e) => {
            warn!("Unable to look for works: {e}");
            false
        }
    }
}

// The works of the albums of the grid with a recording that the filter
// matches, by their composers' sort names and then their names, with the
// numbers in them in order, e.g. "no. 2" before "no. 10"; the works
// without a composer are last. The tracks in the `hidden` folders are left
// out
pub fn works(filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<WorkInfo> {
    match with_connection(|conn| query_works(conn, filter, hidden)) {
        Ok(works) => works,
        Err(e) => {
            warn!("Unable to list the works: {e}");
            Vec::new()
        }
    }
}

fn query_works(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkInfo>> {
    let rows = query_work_rows(conn, filter, None, hidden)?;
    Ok(works_of_rows(&rows))
}

// The works of the rows with a recording that the filter matches, sorted
// as in `works()`
fn works_of_rows(rows: &[WorkRow]) -> Vec<WorkInfo> {
    // The recordings of a work that the filter matches, and how often each
    // of its names is given, by all its tracks
    #[derive(Default)]
    struct Seen {
        recordings: u32,
        names: HashMap<String, usize>,
        titles: HashMap<String, usize>,
        collections: HashMap<String, usize>,
        composers: HashMap<String, usize>,
    }
    let count = |counts: &mut HashMap<String, usize>, value: &Option<String>| {
        if let Some(value) = value {
            *counts.entry(value.clone()).or_default() += 1;
        }
    };

    let mut seen: HashMap<String, Seen> = HashMap::new();
    for recording in recordings_of_works(rows) {
        let rows: Vec<&WorkRow> = recording.into_iter().map(|i| &rows[i]).collect();
        let Some(key) = &rows[0].work else {
            continue;
        };
        let work = seen.entry(key.clone()).or_default();
        if rows.iter().any(|row| row.matching) {
            work.recordings += 1;
        }
        for row in rows {
            count(&mut work.names, &row.name);
            count(&mut work.titles, &row.title);
            count(&mut work.collections, &row.collection);
            count(&mut work.composers, &row.composers);
        }
    }

    let most = |counts: HashMap<String, usize>| Some(most_common(counts)).filter(|s| !s.is_empty());
    let mut works: Vec<(_, WorkInfo)> = seen
        .into_iter()
        .filter(|(_, seen)| seen.recordings > 0)
        .map(|(key, seen)| {
            let composers = most(seen.composers)
                .map(|entries| work_composers(&entries))
                .unwrap_or_default();
            let info = WorkInfo {
                key,
                name: most(seen.names).unwrap_or_default(),
                title: most(seen.titles),
                collection: most(seen.collections),
                composers,
                recordings: seen.recordings,
            };
            let sort_key = (
                info.composers.is_empty(),
                info.composers
                    .first()
                    .map(|composer| glib::CollationKey::from(composer.sort.as_str())),
                info.composers.iter().map(|c| c.key.as_str()).join("\x1f"),
                glib::FilenameCollationKey::from(info.name.as_str()),
            );
            (sort_key, info)
        })
        .collect();
    works.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.key.cmp(&b.1.key)));
    works.into_iter().map(|(_, info)| info).collect()
}

// The work of a track, if it's part of one in the library, and the number
// of its other recordings, without the tracks in the `hidden` folders
pub fn track_work(path: &Path, hidden: &[PathBuf]) -> Option<(WorkInfo, u32)> {
    match with_connection(|conn| query_track_work(conn, path, hidden)) {
        Ok(work) => work,
        Err(e) => {
            warn!("Unable to look up the work of a track: {e}");
            None
        }
    }
}

fn query_track_work(
    conn: &Connection,
    path: &Path,
    hidden: &[PathBuf],
) -> rusqlite::Result<Option<(WorkInfo, u32)>> {
    let key: Option<String> = conn
        .query_row(
            "SELECT work_key FROM tracks WHERE path = ?1 AND work_key IS NOT NULL",
            [path_bytes(path)],
            |row| row.get(0),
        )
        .optional()?;
    let Some(key) = key else {
        return Ok(None);
    };
    let rows = query_work_rows(conn, &TrackFilter::default(), Some(&key), hidden)?;
    let others = recordings_of_works(&rows)
        .into_iter()
        .filter(|recording| {
            rows[recording[0]].work.as_deref() == Some(key.as_str())
                && !recording.iter().any(|&i| rows[i].path == path)
        })
        .count();
    let work = works_of_rows(&rows)
        .into_iter()
        .find(|work| work.key == key);
    Ok(work.map(|work| (work, others as u32)))
}

// The recordings of a work that the filter matches, with all their
// tracks, see `recordings_of_works()`, by the year of their albums, those without one last, and then
// by the albums' titles. The tracks in the `hidden` folders are left out
pub fn work_recordings(key: &str, filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<WorkRecording> {
    match with_connection(|conn| query_work_recordings(conn, key, filter, hidden)) {
        Ok(recordings) => recordings,
        Err(e) => {
            warn!("Unable to list the recordings of a work: {e}");
            Vec::new()
        }
    }
}

fn query_work_recordings(
    conn: &Connection,
    key: &str,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkRecording>> {
    let rows = query_work_rows(conn, filter, Some(key), hidden)?;
    let mut recordings: Vec<(_, WorkRecording)> = recordings_of_works(&rows)
        .into_iter()
        .map(|recording| recording.into_iter().map(|i| &rows[i]).collect::<Vec<_>>())
        .filter(|rows| rows[0].work.as_deref() == Some(key) && rows.iter().any(|row| row.matching))
        .map(|rows| {
            let year = rows.iter().filter_map(|row| row.year.clone()).min();
            let album = rows.iter().find_map(|row| row.album.clone());
            let recording = WorkRecording {
                album_id: rows[0].album_id.clone(),
                album: album.clone(),
                year: year.clone(),
                performers: performers_line(
                    rows.iter().filter_map(|row| row.performers.as_deref()),
                ),
                paths: rows.iter().map(|row| row.path.clone()).collect(),
            };
            let sort_key = (
                year.is_none(),
                year,
                glib::CollationKey::from(album.as_deref().unwrap_or_default()),
                rows[0].album_id.clone(),
            );
            (sort_key, recording)
        })
        .collect();
    recordings.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(recordings
        .into_iter()
        .map(|(_, recording)| recording)
        .collect())
}

// The tracks of the albums with a work, or with the work of `key`, in the
// order of their albums. All their tracks are read, as the tracks between
// the movements of a work split its recordings
fn query_work_rows(
    conn: &Connection,
    filter: &TrackFilter,
    key: Option<&str>,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkRow>> {
    use rusqlite::types::Value;

    let (conditions, values) = filter.conditions();
    let key_sql = if key.is_some() {
        " AND work_key = ?"
    } else {
        ""
    };
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, path, work_key, movement_order,
                CASE WHEN work_key IS NOT NULL{conditions} THEN 1 ELSE 0 END,
                work, work_title, work_collection, work_composers, album, year, performers,
                disc, track
         FROM tracks
         WHERE album_id IN (SELECT album_id FROM tracks WHERE work_key IS NOT NULL{key_sql})
               {hidden_sql}"
    ))?;
    let params = values
        .into_iter()
        .map(Value::Text)
        .chain(key.map(|key| Value::Text(key.to_string())))
        .chain(hidden_params.into_iter().map(Value::Blob));
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
            year: row.get(10)?,
            performers: row.get(11)?,
        };
        Ok((track_order, row))
    })?;
    let mut rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    rows.sort_by(|(a_order, a), (b_order, b)| (&a.album_id, a_order).cmp(&(&b.album_id, b_order)));
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

// The recordings of the works, as the indices of their rows, which are in
// the order of their albums: the tracks of an album with the same work.
// They're not always in the order of the work, as in The Art of Fugue, nor
// next to each other, as in recitals that mix the pieces of a collection
// with others, and MusicBrainz links some of the tracks of an opera to the
// whole opera rather than to its scenes, so they make one recording,
// unless a movement comes again, as on an album with two recordings of the
// work. The parts of a movement in several tracks have its place, one
// after the other
fn recordings_of_works(rows: &[WorkRow]) -> Vec<Vec<usize>> {
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
        let Some(work) = row.work.as_deref() else {
            continue;
        };
        let continues = open.get(work).is_some_and(|open| match &row.order {
            Some(place) if !place.is_empty() => {
                let last = &rows[*recordings[open.recording].last().unwrap()];
                last.order.as_ref() == Some(place) || !open.places.contains(place.as_slice())
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
    for line in entries.flat_map(str::lines) {
        let mut fields = line.split('\t');
        let (Some(kind), Some(key), Some(name)) = (
            fields.next().and_then(performer_kind_from_code),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        if !seen.insert(key.to_string()) {
            continue;
        }
        match kind {
            PerformerKind::Conductor => conductors.push(name),
            PerformerKind::Soloist => soloists.push(name),
            PerformerKind::Ensemble => groups.push(name),
        }
    }
    let performers = soloists.into_iter().chain(groups).join(", ");
    [conductors.join(", "), performers]
        .into_iter()
        .filter(|s| !s.is_empty())
        .join(" · ")
}

// The files of the tracks of an album, in order; with a filter, only the
// tracks it matches, as in `search_albums()`
pub fn album_tracks(album_id: &str, filter: &TrackFilter) -> Vec<PathBuf> {
    let query = |conn: &Connection| -> rusqlite::Result<Vec<PathBuf>> {
        let (conditions, values) = filter.conditions();
        let mut stmt = conn.prepare(&format!(
            "SELECT path, disc, track FROM tracks
             WHERE album_id = ?{conditions}"
        ))?;
        let params = std::iter::once(album_id).chain(values.iter().map(String::as_str));
        let mut tracks = stmt
            .query_map(rusqlite::params_from_iter(params), |row| {
                let path = path_from_bytes(row.get(0)?);
                Ok((TrackOrder::new(&path, row.get(1)?, row.get(2)?), path))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tracks.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(tracks.into_iter().map(|(_, path)| path).collect())
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
    let mut result = scan_internal(folders, thumbnail_size, sender).unwrap_or_else(|e| {
        warn!("Unable to scan the library: {e}");
        ScanResult::default()
    });
    debug!("Library scan: {} ms", now.elapsed().as_millis());
    // Even when the database couldn't be read, the folders are watched
    result.watch = folders_to_watch(folders, &roots(folders));

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

// The folders of the library to watch for files that come, go or change:
// those in the music folders on this computer's disks, with the folders in
// them and those that symbolic links in them lead to, as the scan reads
// them, but not those that are hidden or left out, which it doesn't.
// Changes in a folder on a network share can be made from other
// computers, which aren't told about, and the folders that came through
// the document portal aren't watched either. Also the music folders and
// the folders that links lead to, for the drives mounted and unmounted,
// see `WatchList`. This blocks
fn folders_to_watch(folders: &[MusicFolder], roots: &Roots) -> WatchList {
    let mut list = WatchList {
        unavailable: !roots.unavailable.is_empty(),
        ..WatchList::default()
    };
    let canonical = |path: PathBuf| std::fs::canonicalize(&path).unwrap_or(path);
    for root in &roots.available {
        list.available.push(canonical(root.clone()));
        let portal = folders
            .iter()
            .any(|folder| folder.path == *root && folder.host.is_some());
        if portal || is_remote(root) {
            debug!("Library: not watching {root:?}");
            list.unwatched = true;
            continue;
        }
        let tree = utils::read_folder_tree(&gio::File::for_path(root), true, &|path| {
            scan_reads(path, roots)
        });
        // A link to a drive that isn't mounted leads nowhere until it is
        list.unavailable |= tree.broken_links;
        let mut remote = Vec::new();
        for link in tree.links.iter().filter_map(gio::File::path) {
            if is_remote(&link) {
                debug!("Library: not watching {link:?}");
                list.unwatched = true;
                remote.push(link.clone());
            }
            list.available.push(canonical(link));
        }
        list.folders.extend(
            tree.folders
                .iter()
                .filter_map(gio::File::path)
                .filter(|folder| !remote.iter().any(|link| folder.starts_with(link))),
        );
    }
    list
}

fn is_remote(path: &Path) -> bool {
    gio::File::for_path(path)
        .query_filesystem_info(
            gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
            None::<&gio::Cancellable>,
        )
        .is_ok_and(|info| info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE))
}

// Whether the scan reads a file or a folder it comes to in the music
// folders: not a hidden one, nor a folder left out. The folders watched
// are the ones it reads, so both go by this
fn scan_reads(path: &Path, roots: &Roots) -> bool {
    is_library_entry(path) && !roots.excluded.iter().any(|excluded| excluded == path)
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
                scan_reads(path, roots)
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
                  artist_sort, year, disc, track, title, search, added, composers, work_key,
                  work, work_title, work_collection, work_composers, movement_order, performers,
                  label, catalog_number)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
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

                    let work = tags.work.as_ref();
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
                        tags.composers,
                        work.map(|w| &w.key),
                        work.map(|w| &w.name),
                        work.and_then(|w| w.title.as_ref()),
                        work.and_then(|w| w.collection.as_ref()),
                        work.and_then(|w| w.composers.as_ref()),
                        work.and_then(|w| w.order.as_ref()),
                        tags.performers,
                        tags.label,
                        tags.catalog_number,
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
            ..ScanResult::default()
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

// When the newest cover file for a folder of tracks was changed, if it
// has any; the thumbnail is made from the first track of the album, which
// takes its cover from these files when its tags have none
fn cover_file_time(dir: &Path) -> Option<std::time::SystemTime> {
    CoverCache::cover_files_for(dir)
        .filter_map(|file| std::fs::metadata(file).ok()?.modified().ok())
        .max()
}

// Makes the thumbnail of an album in `dir`, from the cover of its first
// track, and returns its path
fn create_thumbnail(track: &Path, dir: &Path, album_id: &str, size: u32) -> Option<PathBuf> {
    let track = cue_sheet::audio_path(track)?;
    let track = track.as_path();
    let tags = read_file_tags(track, ReadOptions::ALL)?;
    // A file without tags can still have a cover file next to it
    let image = CoverCache::load_cover(&tags.tag, track.parent(), size)?;
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
fn read_tags_only(path: &Path) -> Option<FileTags> {
    read_file_tags(path, ReadOptions::TAGS)
}

// The tracks of an audio file: the file, or the tracks that the CUE sheet
// in its tags splits it into
fn read_tags(path: &Path) -> Vec<(PathBuf, TrackTags)> {
    let Some(FileTags { tag, extra, .. }) = read_tags_only(path) else {
        return vec![(path.to_path_buf(), TrackTags::default())];
    };

    let sheet = extra
        .cue_sheet
        .as_deref()
        .and_then(|text| SplitSheet::embedded(path, text));
    if let Some(sheet) = sheet {
        let album_tag = cue_sheet::album_tag(Some(&tag), &sheet.sheet);
        return sheet
            .tracks()
            .map(|(track, _)| cue_track_tags(&sheet, &album_tag, track))
            .collect();
    }

    // A file without tags is in the album of its folder too
    let tags = TrackTags::from_tag(&tag, path, Some(path), &extra);
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
                cue_sheet::album_tag(file.as_ref().map(|file| &file.tag), &sheet.sheet)
            });
            cue_track_tags(sheet, album_tag, track)
        })
        .collect()
}

fn cue_track_tags(sheet: &SplitSheet, album_tag: &Tag, track: &CueTrack) -> (PathBuf, TrackTags) {
    let tag = cue_sheet::track_tag(album_tag, &sheet.sheet, track);
    // The album is in the folder of the sheet, and the items that lofty
    // doesn't read are about the whole audio file
    let tags = TrackTags::from_tag(&tag, &sheet.path, None, &ExtraComments::default());
    (sheet.location(track), tags)
}

// MusicBrainz ids are UUIDs; the album id names the file of its cover, so
// anything else in the tag, like a path, must not be used as one
pub fn is_mbid(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

// Whether a folder holds a disc of an album, e.g. "CD 1", "Disc 2 - Live"
// or "Disque 3", in a few languages: a word for a disc, then its number,
// of up to three digits, as box sets can have a hundred discs, but "Disco
// 1980" isn't one, and then anything after a space or a separator
pub fn is_disc_folder(name: &std::ffi::OsStr) -> bool {
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

// The folder of the album a track is in; the discs of an album are often
// in their own folders
fn album_folder(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    if is_disc_folder(parent.file_name()?) {
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
    // have none. The items that lofty doesn't read, like the sort names of
    // the composers and the "Work Hierarchy" tag, are in `extra`
    fn from_tag(tag: &Tag, path: &Path, file: Option<&Path>, extra: &ExtraComments) -> Self {
        let get = |key| tag.get_string(key).map(|s| s.trim().to_string());

        let album = get(ItemKey::AlbumTitle).filter(|s| !s.is_empty());
        let album_artist = get(ItemKey::AlbumArtist).filter(|s| !s.is_empty());
        let artist = tag
            .artist()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let artist_sort = get(ItemKey::AlbumArtistSortOrder).filter(|s| !s.is_empty());

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

        let credits = TrackCredits::from_tag(tag, extra);

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
            &track_search_text(tag, &credits),
            folder_album.as_deref(),
        );

        let names = split_composers(tag.get_strings(ItemKey::Composer));
        let sorts = split_composers(tag.get_strings(ItemKey::ComposerSortOrder));

        TrackTags {
            album_id: Some(album_id),
            album: album.or(folder_album),
            // After the id, which must not change
            album_artist: album_artist.as_deref().map(plain_commas),
            artist: artist.as_deref().map(plain_commas),
            artist_sort,
            year: credits.year(),
            disc: tag.disk(),
            track: tag.track(),
            title,
            search: Some(search),
            composers: composer_entries(&names, &sorts),
            work: work_entry(&credits, artist.as_deref(), &names, &sorts),
            performers: performer_entries(&credits.listed_performers()),
            label: credits.labels.first().cloned(),
            catalog_number: credits.catalog_numbers.first().cloned(),
        }
    }
}

// The work a track performs, or a movement of. The work is told apart by
// its MusicBrainz id: the one the "Work Hierarchy" plugin picks, or else,
// for a track of a whole work, the work it's linked to, which for a
// movement would be the movement's. Without one, works with the same name
// and composers are the same, as the queue's headers take them to be
fn work_entry(
    credits: &TrackCredits,
    artist: Option<&str>,
    names: &[String],
    sorts: &[String],
) -> Option<WorkEntry> {
    let name = credits.work.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    let is_movement = credits.movement_label().is_some();

    // The composers of the work, as `Track::work_composers()` finds them,
    // sorted by their own sort names if the tags have them
    let mut work_names = if credits.work_composers.is_empty() {
        credits.composers.clone()
    } else {
        credits.work_composers.clone()
    };
    if work_names.is_empty() {
        work_names.extend(artist.map(ToString::to_string));
    }
    let sort_of = |composer: &String| {
        names
            .iter()
            .position(|name| composer_key(name) == composer_key(composer))
            .and_then(|i| sorts.get(i))
            .filter(|_| sorts.len() == names.len())
            .cloned()
            .unwrap_or_else(|| sort_name(composer))
    };
    let work_sorts = work_names.iter().map(sort_of).collect::<Vec<_>>();

    let key = credits
        .work_id
        .clone()
        .or_else(|| credits.musicbrainz.work.clone().filter(|_| !is_movement))
        .unwrap_or_else(|| {
            let composers = work_names
                .iter()
                .map(|name| composer_key(name))
                .sorted()
                .join("\x1f");
            format!("t:{}\x1f{composers}", composer_key(name))
        });
    let order = credits
        .movement_order
        .as_ref()
        .filter(|_| is_movement)
        .map(|order| order.iter().join("."));
    Some(WorkEntry {
        key,
        name: name.to_string(),
        title: credits.work_title.clone(),
        collection: credits.work_collection.clone(),
        composers: composer_entries(&work_names, &work_sorts),
        order,
    })
}

// The conductors and performers of a track, as the library keeps them: a
// line for each, with its kind ("c", "e" or "s", see `PerformerKind`), its
// key, as for composers, its name, the name it's sorted by, and its role,
// separated by tabs, between line breaks, so that a filter finds a
// performer by "\t{key}\t", whatever its kind (`TrackFilter`). People are
// sorted by their last names, and groups by their names
fn performer_entries(performers: &[ListedPerformer]) -> Option<String> {
    if performers.is_empty() {
        return None;
    }
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut entries = String::from("\n");
    for performer in performers {
        let sort = match performer.kind {
            PerformerKind::Ensemble => performer.name.clone(),
            PerformerKind::Conductor | PerformerKind::Soloist => sort_name(&performer.name),
        };
        entries.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            performer_kind_code(performer.kind),
            composer_key(&performer.name),
            clean(&performer.name),
            clean(&sort),
            clean(performer.role.as_deref().unwrap_or_default()),
        ));
    }
    Some(entries)
}

// How the library writes the kind of a performer
fn performer_kind_code(kind: PerformerKind) -> char {
    match kind {
        PerformerKind::Conductor => 'c',
        PerformerKind::Ensemble => 'e',
        PerformerKind::Soloist => 's',
    }
}

fn performer_kind_from_code(code: &str) -> Option<PerformerKind> {
    match code {
        "c" => Some(PerformerKind::Conductor),
        "e" => Some(PerformerKind::Ensemble),
        "s" => Some(PerformerKind::Soloist),
        _ => None,
    }
}

// The composers of a track, as the library keeps them: a line for each,
// with its key, which tells composers apart ignoring case and accents, its
// name and the name it's sorted by, separated by tabs, between line
// breaks, so that a filter finds a composer by "\n{key}\t" (`TrackFilter`).
// The sort names go with the names in their order, when there are as many;
// otherwise each is left empty, and worked out from its name when the
// composers are sorted, see `composer_sort()`
fn composer_entries(names: &[String], sorts: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut entries = String::from("\n");
    for (i, name) in names.iter().enumerate() {
        let sort = sorts
            .get(i)
            .filter(|_| sorts.len() == names.len())
            .map_or("", String::as_str);
        entries.push_str(&format!(
            "{}\t{}\t{}\n",
            composer_key(name),
            clean(name),
            clean(sort)
        ));
    }
    Some(entries)
}

// What tells a composer apart: the name, ignoring case and accents, and
// how it's spaced
pub fn composer_key(name: &str) -> String {
    normalize_search_text(name)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// The name a composer is sorted by: the sort name of their tags, or else
// one worked out from their name
fn composer_sort(name: &str, sort: &str) -> String {
    if sort.is_empty() {
        sort_name(name)
    } else {
        sort.to_string()
    }
}

// The name a composer without a sort name is sorted by: the last name
// first, as in "Beethoven, Ludwig van", keeping a number or "Jr." after
// it, as in "Strauss, Johann II". A name with a comma is taken to be
// written that way already, as some taggers do
fn sort_name(name: &str) -> String {
    if name.contains(',') {
        return name.trim().to_string();
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    let suffix = words.len() > 2
        && matches!(
            words.last().copied(),
            Some("I" | "II" | "III" | "IV" | "Jr." | "Jr" | "Sr." | "Sr")
        );
    let (rest, last) = match words.len() {
        0 | 1 => return name.trim().to_string(),
        n if suffix => ([&words[..n - 2], &words[n - 1..]].concat(), words[n - 2]),
        n => (words[..n - 1].to_vec(), words[n - 1]),
    };
    format!("{last}, {}", rest.join(" "))
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
        let tags =
            super::TrackTags::from_tag(&tag, path, Some(path), &super::ExtraComments::default());
        assert_eq!(tags.album.as_deref(), Some("Some Folder"));
        let search = tags.search.unwrap();
        assert!(search.contains("intro"));
        assert!(search.contains("some folder"));

        tag.insert_text(ItemKey::AlbumTitle, "Album".to_string());
        let tags =
            super::TrackTags::from_tag(&tag, path, Some(path), &super::ExtraComments::default());
        assert_eq!(tags.title, None);
        let search = tags.search.unwrap();
        assert!(search.contains("intro"));
        assert!(!search.contains("some folder"));

        tag.insert_text(ItemKey::TrackTitle, "Overture".to_string());
        let tags =
            super::TrackTags::from_tag(&tag, path, Some(path), &super::ExtraComments::default());
        let search = tags.search.unwrap();
        assert!(search.contains("overture"));
        assert!(!search.contains("intro"));
    }

    #[test]
    fn works_are_told_apart_by_their_ids_or_names_and_composers() {
        use lofty::tag::{ItemKey, Tag, TagType};

        use super::ExtraComments;

        let path = Path::new("/music/Requiem/01.flac");
        let work = |work: &str, composers: &[&str], extra: &ExtraComments| {
            let mut tag = Tag::new(TagType::VorbisComments);
            tag.insert_text(ItemKey::Work, work.to_string());
            for composer in composers {
                tag.push(lofty::tag::TagItem::new(
                    ItemKey::Composer,
                    lofty::tag::ItemValue::Text((*composer).to_string()),
                ));
            }
            super::TrackTags::from_tag(&tag, path, Some(path), extra)
                .work
                .unwrap()
        };
        let none = ExtraComments::default();

        // Without an id, the same name and composers, ignoring case and
        // accents, are the same work, and other composers make another
        let requiem = work("Requiem, op. 89", &["Antonín Dvořák"], &none);
        assert_eq!(
            requiem.key,
            work("requiem, Op. 89", &["Antonin Dvorak"], &none).key
        );
        assert_ne!(
            requiem.key,
            work("Requiem, op. 89", &["Someone Else"], &none).key
        );
        assert_eq!(requiem.name, "Requiem, op. 89");
        assert_eq!(
            requiem.composers.as_deref(),
            Some("\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n")
        );
        // A whole work has no order
        assert_eq!(requiem.order, None);

        // The hierarchy's id, whatever the work tag says, and the place of
        // the movement
        let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0,
            "path": [
                { "id": "1d6a0d5c-0000-0000-0000-000000000000", "title": "Requiem in B-flat minor",
                  "name": "Requiem in B-flat minor",
                  "composers": [{ "id": "a", "name": "Antonín Dvořák" }] },
                { "id": "s", "title": "…: Sequentia", "name": "Sequentia", "position": 3 },
                { "id": "d", "title": "…: Dies irae", "name": "Dies irae", "position": 1 },
            ] }] });
        let extra = ExtraComments::with_work_hierarchy(&hierarchy.to_string());
        let movement = work("Requiem v B-moll, op. 89", &["Antonín Dvořák"], &extra);
        assert_eq!(movement.key, "1d6a0d5c-0000-0000-0000-000000000000");
        assert_eq!(movement.name, "Requiem in B-flat minor");
        assert_eq!(movement.order.as_deref(), Some("3.1"));
    }

    #[test]
    fn performers_are_kept_with_their_kinds_and_roles() {
        use crate::audio::{ListedPerformer, PerformerKind};

        let performer = |kind, name: &str, role: Option<&str>| ListedPerformer {
            kind,
            name: name.to_string(),
            role: role.map(ToString::to_string),
        };
        assert_eq!(super::performer_entries(&[]), None);
        assert_eq!(
            super::performer_entries(&[
                performer(PerformerKind::Conductor, "Philippe Herreweghe", None),
                performer(
                    PerformerKind::Ensemble,
                    "Collegium Vocale Gent",
                    Some("choir vocals")
                ),
                performer(
                    PerformerKind::Soloist,
                    "Ilse Eerens",
                    Some("soprano vocals")
                ),
            ])
            .as_deref(),
            Some(
                "\nc\tphilippe herreweghe\tPhilippe Herreweghe\tHerreweghe, Philippe\t\n\
                 e\tcollegium vocale gent\tCollegium Vocale Gent\tCollegium Vocale Gent\t\
                 choir vocals\n\
                 s\tilse eerens\tIlse Eerens\tEerens, Ilse\tsoprano vocals\n"
            )
        );
    }

    #[test]
    fn tracks_are_searched_by_the_names_in_the_work_hierarchy() {
        use lofty::tag::{ItemKey, Tag, TagType};

        let path = Path::new("/music/WTC/01.flac");
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(
            ItemKey::Work,
            "Präludium und Fuge C-Dur, BWV 846".to_string(),
        );
        let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 1,
            "path": [
                { "id": "b", "title": "The Well-Tempered Clavier, Book I", "name": "Book I" },
                { "id": "p", "title": "…: Prelude and Fugue in C major, BWV 846",
                  "name": "Prelude and Fugue in C major, BWV 846", "number": 1 },
                { "id": "f", "title": "…: Fugue", "name": "Fugue", "number": 2 },
            ] }] });
        let extra = super::ExtraComments::with_work_hierarchy(&hierarchy.to_string());
        let search = super::TrackTags::from_tag(&tag, path, Some(path), &extra)
            .search
            .unwrap();
        assert!(search.contains("praludium und fuge"));
        assert!(search.contains("well-tempered clavier"));
        assert!(search.contains("prelude and fugue in c major"));
    }

    #[test]
    fn disc_folders_have_names_in_several_languages() {
        use super::is_disc_folder;
        use std::ffi::OsStr;

        for name in [
            "CD1",
            "CD 2",
            "cd_03",
            "Disc 1",
            "Disc 1 - Live at the Proms",
            "Disc 2 of 2",
            "CD1 (Bonus)",
            "Disk.4",
            "Disco 2",
            "Disque 1",
            "Schijf 2",
            "Płyta 1",
            "CD 153",
        ] {
            assert!(is_disc_folder(OsStr::new(name)), "{name}");
        }
        for name in [
            "CD Collection",
            "Discography",
            "Disco 1980",
            "Disco 1980 Hits",
            "CD1s",
            "Disc One",
            "Album",
            "1",
        ] {
            assert!(!is_disc_folder(OsStr::new(name)), "{name}");
        }
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
            let mut albums: Vec<(String, (u32, u32))> = super::query_search_albums(
                &conn,
                &super::TrackFilter {
                    words,
                    ..Default::default()
                },
                &hidden,
            )
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

    // As in a library that beets makes of links, or with a folder on
    // another drive linked into the music folder
    #[cfg(unix)]
    #[test]
    fn symbolic_links_in_a_music_folder_are_followed_once() {
        use std::os::unix::fs::symlink;

        use super::scan_with;
        use crate::utils::write_test_wav;

        let music = test_dir("links");
        std::fs::create_dir_all(music.join("Real")).unwrap();
        write_test_wav(&music.join("Real/One.wav"), 1);
        let elsewhere = test_dir("links-elsewhere");
        write_test_wav(&elsewhere.join("Two.wav"), 1);
        symlink(&elsewhere, music.join("Linked")).unwrap();
        let other = test_dir("links-other");
        write_test_wav(&other.join("Three.wav"), 1);
        symlink(other.join("Three.wav"), music.join("Single.wav")).unwrap();
        // Links to what the folder has already, and a loop, add nothing
        symlink(music.join("Real"), music.join("Again")).unwrap();
        symlink(music.join("Real/One.wav"), music.join("Real/Copy.wav")).unwrap();
        symlink(&music, music.join("Real/Loop")).unwrap();
        // Nor does a link to nothing
        symlink(music.join("Nothing"), music.join("Gone")).unwrap();

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        scan_with(&mut conn, &roots_of(&[&music]), &sender).unwrap();
        let found: Vec<String> = tracks(&conn, &music)
            .into_iter()
            .map(|row| row.path)
            .collect();
        assert_eq!(found, ["Linked/Two.wav", "Real/One.wav", "Single.wav"]);

        std::fs::remove_dir_all(&music).unwrap();
        std::fs::remove_dir_all(&elsewhere).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn trash_and_hidden_folders_are_not_read() {
        use super::scan_with;
        use crate::utils::write_test_wav;

        let dir = test_dir("hidden");
        for folder in [
            "Album",
            ".Trash-1000/files",
            "$Recycle.Bin",
            "#recycle",
            "#snapshot/daily",
            "Album/@eaDir",
        ] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        write_test_wav(&dir.join("Album/One.wav"), 1);
        write_test_wav(&dir.join("Album/.Two.wav"), 1);
        write_test_wav(&dir.join(".Trash-1000/files/Three.wav"), 1);
        write_test_wav(&dir.join("$Recycle.Bin/$R4ND0M.wav"), 1);
        write_test_wav(&dir.join("#recycle/Four.wav"), 1);
        write_test_wav(&dir.join("#snapshot/daily/One.wav"), 1);
        write_test_wav(&dir.join("Album/@eaDir/One.wav"), 1);

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

    #[test]
    fn the_folders_the_scan_reads_are_watched() {
        let music = test_dir("watch");
        for folder in [
            "Bach/Partitas/CD1",
            "Bach/Partitas/CD2",
            "Audiobooks/A",
            ".Trash-1000/x",
        ] {
            std::fs::create_dir_all(music.join(folder)).unwrap();
        }
        std::fs::write(music.join("Bach/Partitas/CD1/01.opus"), b"").unwrap();
        // A link to a folder elsewhere is watched as the scan reads it, and
        // one back into the music folder adds nothing
        let elsewhere = test_dir("watch-elsewhere");
        std::fs::create_dir_all(elsewhere.join("Jazz")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, music.join("Linked")).unwrap();
        std::os::unix::fs::symlink(&music, music.join("Bach/Partitas/Back")).unwrap();

        let mut folder = MusicFolder::new(music.clone());
        folder.excluded.push(PathBuf::from("Audiobooks"));
        let folders = [folder];
        let list = super::folders_to_watch(&folders, &super::roots(&folders));
        let mut watched = list.folders;
        watched.sort();
        let relative: Vec<&Path> = watched
            .iter()
            .map(|path| path.strip_prefix(&music).unwrap())
            .collect();
        assert_eq!(
            relative,
            [
                Path::new(""),
                Path::new("Bach"),
                Path::new("Bach/Partitas"),
                Path::new("Bach/Partitas/CD1"),
                Path::new("Bach/Partitas/CD2"),
                Path::new("Linked"),
                Path::new("Linked/Jazz"),
            ]
        );
        assert!(!list.unwatched);
        assert_eq!(
            list.available,
            [
                music.canonicalize().unwrap(),
                elsewhere.canonicalize().unwrap()
            ]
        );
        assert!(!list.unavailable);

        // A link that leads nowhere can be to a drive that isn't mounted
        let gone = music.join("Gone");
        std::os::unix::fs::symlink(music.join("Nothing"), &gone).unwrap();
        let list = super::folders_to_watch(&folders, &super::roots(&folders));
        assert!(list.unavailable);
        std::fs::remove_file(gone).unwrap();

        // A folder that came through the document portal isn't watched
        let folders = [MusicFolder {
            host: Some(PathBuf::from("/media/music")),
            ..MusicFolder::new(music.clone())
        }];
        let list = super::folders_to_watch(&folders, &super::roots(&folders));
        assert!(list.folders.is_empty());
        assert!(list.unwatched);

        // A drive mounted anywhere can bring back a folder that isn't there
        let folders = [MusicFolder::new(music.join("Unplugged"))];
        let list = super::folders_to_watch(&folders, &super::roots(&folders));
        assert!(list.folders.is_empty());
        assert!(list.unavailable);

        std::fs::remove_dir_all(music).unwrap();
        std::fs::remove_dir_all(elsewhere).unwrap();
    }

    #[test]
    fn the_tracks_of_each_album_are_in_their_order() {
        let conn = test_db();
        for (path, album, disc, track) in [
            ("/m/B/2-01.flac", "b", 2, 1),
            ("/m/A/02.flac", "a", 1, 2),
            ("/m/B/1-01.flac", "b", 1, 1),
            ("/m/A/01.flac", "a", 1, 1),
            ("/m/Gone/01.flac", "c", 1, 1),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, disc, track)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![path.as_bytes(), album, disc, track],
            )
            .unwrap();
        }
        let albums = super::query_album_track_paths(&conn, &[PathBuf::from("/m/Gone")]).unwrap();
        let paths = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            albums,
            [
                ("a".to_string(), paths(&["/m/A/01.flac", "/m/A/02.flac"])),
                (
                    "b".to_string(),
                    paths(&["/m/B/1-01.flac", "/m/B/2-01.flac"])
                ),
            ]
        );
    }

    #[test]
    fn tracks_without_numbers_are_in_the_order_of_their_names() {
        let conn = test_db();
        let add = |path: &str, album: &str, track: Option<i64>| {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, track)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
                rusqlite::params![path.as_bytes(), album, track],
            )
            .unwrap();
        };
        // Numbers in their order; case goes by the locale, which is C in
        // the tests
        for name in ["10 Ten", "b", "2 Two", "1 One", "a"] {
            add(&format!("/m/Untagged/{name}.flac"), "untagged", None);
        }
        // Discs in folders of their own, without disc numbers
        for (path, track) in [
            ("/m/Set/CD2/01.flac", 1),
            ("/m/Set/CD1/02.flac", 2),
            ("/m/Set/CD2/02.flac", 2),
            ("/m/Set/CD1/01.flac", 1),
        ] {
            add(path, "set", Some(track));
        }

        let names = |album: &str| -> Vec<String> {
            super::query_album_track_paths(&conn, &[])
                .unwrap()
                .into_iter()
                .find(|(id, _)| id == album)
                .unwrap()
                .1
                .iter()
                .map(|path| path.strip_prefix("/m").unwrap().display().to_string())
                .collect()
        };
        assert_eq!(
            names("untagged"),
            [
                "Untagged/1 One.flac",
                "Untagged/2 Two.flac",
                "Untagged/10 Ten.flac",
                "Untagged/a.flac",
                "Untagged/b.flac",
            ]
        );
        assert_eq!(
            names("set"),
            [
                "Set/CD1/01.flac",
                "Set/CD1/02.flac",
                "Set/CD2/01.flac",
                "Set/CD2/02.flac",
            ]
        );
    }

    #[test]
    fn composers_without_a_sort_name_are_sorted_by_their_last_name() {
        use super::sort_name;
        assert_eq!(sort_name("Johann Sebastian Bach"), "Bach, Johann Sebastian");
        assert_eq!(sort_name("Ludwig van Beethoven"), "Beethoven, Ludwig van");
        assert_eq!(sort_name("Johann Strauss II"), "Strauss, Johann II");
        assert_eq!(sort_name("Pérotin"), "Pérotin");
        assert_eq!(
            sort_name("Bach, Johann Sebastian"),
            "Bach, Johann Sebastian"
        );
    }

    #[test]
    fn the_composers_of_a_track_keep_their_sort_names() {
        use super::composer_entries;
        let names = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(composer_entries(&[], &[]), None);
        assert_eq!(
            composer_entries(
                &names(&["Antonín Dvořák", "Josef Suk"]),
                &names(&["Dvořák, Antonín", "Suk, Josef"])
            )
            .as_deref(),
            Some(
                "\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\njosef suk\tJosef Suk\tSuk, Josef\n"
            )
        );
        // Without as many sort names, they're left out, to be worked out
        assert_eq!(
            composer_entries(
                &names(&["Antonín Dvořák", "Josef Suk"]),
                &names(&["Suk, Josef"])
            )
            .as_deref(),
            Some("\nantonin dvorak\tAntonín Dvořák\t\njosef suk\tJosef Suk\t\n")
        );
    }

    #[test]
    fn composers_without_a_sort_tag_are_listed_by_their_name() {
        let conn = test_db();
        for (path, album, composers) in [
            ("/m/A/01.flac", "a", "\npink floyd\tPink Floyd\t\n"),
            ("/m/B/01.flac", "b", "\ntan dun\tTan Dun\t\n"),
            (
                "/m/C/01.flac",
                "c",
                "\nbela bartok\tBéla Bartók\tBartók, Béla\n",
            ),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
                rusqlite::params![path.as_bytes(), album, composers],
            )
            .unwrap();
        }
        // Sorted by the last name all the same, but a sort name worked out
        // isn't shown, as it would turn a band's name around; one from the
        // tags is
        let listed: Vec<(String, String)> =
            super::query_composers(&conn, &[], &super::TrackFilter::default())
                .unwrap()
                .into_iter()
                .map(|composer| (composer.sort, composer.label))
                .collect();
        assert_eq!(
            listed,
            [
                ("Bartók, Béla".to_string(), "Bartók, Béla".to_string()),
                ("Dun, Tan".to_string(), "Tan Dun".to_string()),
                ("Floyd, Pink".to_string(), "Pink Floyd".to_string()),
            ]
        );
    }

    #[test]
    fn composers_are_listed_with_their_albums_and_found_by_their_key() {
        let conn = test_db();
        let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
        let cpe =
            "\ncarl philipp emanuel bach\tCarl Philipp Emanuel Bach\tBach, Carl Philipp Emanuel\n";
        let both = format!("{bach}{}", &cpe[1..]);
        for (path, album, composers, search) in [
            ("/m/A/01.opus", "a", Some(bach), "bwv 1001"),
            ("/m/A/02.opus", "a", Some(bach), "bwv 1002"),
            ("/m/B/01.opus", "b", Some(cpe), "wq 182"),
            ("/m/B/02.opus", "b", Some(both.as_str()), "bwv 1037"),
            ("/m/C/01.opus", "c", None, "anon"),
            ("/m/Gone/01.opus", "d", Some(bach), "bwv 988"),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![path.as_bytes(), album, composers, search],
            )
            .unwrap();
        }
        let hidden = [PathBuf::from("/m/Gone")];
        assert!(super::query_has_composers(&conn, &hidden).unwrap());
        let only_anonymous = [
            PathBuf::from("/m/A"),
            PathBuf::from("/m/B"),
            PathBuf::from("/m/Gone"),
        ];
        assert!(!super::query_has_composers(&conn, &only_anonymous).unwrap());

        let all = super::TrackFilter::default();
        let composers = super::query_composers(&conn, &hidden, &all).unwrap();
        let listed: Vec<(&str, u32)> = composers
            .iter()
            .map(|composer| (composer.sort.as_str(), composer.albums))
            .collect();
        assert_eq!(
            listed,
            [
                ("Bach, Carl Philipp Emanuel", 1),
                ("Bach, Johann Sebastian", 2)
            ]
        );
        // The list's search looks into both names
        assert_eq!(
            composers[1].search,
            "johann sebastian bach bach, johann sebastian"
        );

        // With a filter, only the composers of the tracks it matches, with
        // the albums of those tracks
        let listed = |words: &[&str]| -> Vec<(String, u32)> {
            let filter = super::TrackFilter {
                words: words.iter().map(ToString::to_string).collect(),
                ..Default::default()
            };
            super::query_composers(&conn, &hidden, &filter)
                .unwrap()
                .into_iter()
                .map(|composer| (composer.sort, composer.albums))
                .collect()
        };
        assert_eq!(
            listed(&["1037"]),
            [
                ("Bach, Carl Philipp Emanuel".to_string(), 1),
                ("Bach, Johann Sebastian".to_string(), 1)
            ]
        );
        assert_eq!(
            listed(&["wq"]),
            [("Bach, Carl Philipp Emanuel".to_string(), 1)]
        );
        assert!(listed(&["mozart"]).is_empty());

        // Only Johann Sebastian's, not his son's, though the names share a
        // word, with the tracks that are his
        let filter = super::TrackFilter {
            composer: Some("johann sebastian bach".to_string()),
            ..Default::default()
        };
        let mut albums: Vec<(String, (u32, u32))> =
            super::query_search_albums(&conn, &filter, &hidden)
                .unwrap()
                .into_iter()
                .collect();
        albums.sort();
        assert_eq!(
            albums,
            [("a".to_string(), (2, 2)), ("b".to_string(), (1, 2))]
        );

        // and with the words of a search too
        let filter = super::TrackFilter {
            words: vec!["1002".to_string()],
            ..filter
        };
        let albums = super::query_search_albums(&conn, &filter, &hidden).unwrap();
        assert_eq!(albums.get("a"), Some(&(1, 2)));
        assert_eq!(albums.len(), 1);
    }

    #[test]
    fn performers_are_listed_once_by_what_they_do_the_most() {
        use crate::audio::PerformerKind;

        let conn = test_db();
        let herreweghe = "c\tphilippe herreweghe\tPhilippe Herreweghe\tHerreweghe, Philippe\t";
        let cvg = "e\tcollegium vocale gent\tCollegium Vocale Gent\tCollegium Vocale Gent\t\
                   choir vocals";
        let eerens = "s\tilse eerens\tIlse Eerens\tEerens, Ilse\tsoprano vocals";
        // Named in the album artist of a track she doesn't sing on
        let eerens_guessed = "e\tilse eerens\tIlse Eerens\tIlse Eerens\t";
        let chandler = "c\tadrian chandler\tAdrian Chandler\tChandler, Adrian\t";
        let chandler_solo = "s\tadrian chandler\tAdrian Chandler\tChandler, Adrian\tsolo violin";
        let entries = |lines: &[&str]| format!("\n{}\n", lines.join("\n"));
        for (path, album, performers, composers) in [
            (
                "/m/A/01.opus",
                "a",
                entries(&[herreweghe, cvg, eerens]),
                Some("\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n"),
            ),
            (
                "/m/A/02.opus",
                "a",
                entries(&[herreweghe, eerens_guessed]),
                None,
            ),
            ("/m/B/01.opus", "b", entries(&[herreweghe, cvg]), None),
            (
                "/m/C/01.opus",
                "c",
                entries(&[chandler, chandler_solo]),
                None,
            ),
            ("/m/D/01.opus", "d", entries(&[chandler]), None),
            ("/m/D/02.opus", "d", entries(&[chandler_solo]), None),
            ("/m/Gone/01.opus", "g", entries(&[eerens]), None),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, performers,
                                     composers, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, '')",
                rusqlite::params![path.as_bytes(), album, performers, composers],
            )
            .unwrap();
        }
        let hidden = [PathBuf::from("/m/Gone")];
        assert!(super::query_has_performers(&conn, &hidden).unwrap());

        let all = super::TrackFilter::default();
        let performers = super::query_performers(&conn, &hidden, &all).unwrap();
        let listed: Vec<(PerformerKind, &str, &[String], u32)> = performers
            .iter()
            .map(|p| (p.kind, p.sort.as_str(), p.roles.as_slice(), p.albums))
            .collect();
        let roles = |roles: &[&str]| roles.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            listed,
            [
                // Conducting on as many albums as playing solo
                (
                    PerformerKind::Conductor,
                    "Chandler, Adrian",
                    roles(&["solo violin"]).as_slice(),
                    2
                ),
                (PerformerKind::Conductor, "Herreweghe, Philippe", &[], 2),
                (
                    PerformerKind::Ensemble,
                    "Collegium Vocale Gent",
                    roles(&["choir vocals"]).as_slice(),
                    2
                ),
                // A singer, though the album artist named her on a track
                (
                    PerformerKind::Soloist,
                    "Eerens, Ilse",
                    roles(&["soprano vocals"]).as_slice(),
                    1
                ),
            ]
        );
        // The list's search looks into the names and the roles
        assert_eq!(
            performers[3].search,
            "ilse eerens eerens, ilse soprano vocals"
        );

        // A composer's performers are those of their tracks, with the
        // albums of those tracks, each as what they do the most anywhere
        let filter = super::TrackFilter {
            composer: Some("antonin dvorak".to_string()),
            ..Default::default()
        };
        let dvorak = super::query_performers(&conn, &hidden, &filter).unwrap();
        let listed: Vec<(PerformerKind, &str, u32)> = dvorak
            .iter()
            .map(|p| (p.kind, p.sort.as_str(), p.albums))
            .collect();
        assert_eq!(
            listed,
            [
                (PerformerKind::Conductor, "Herreweghe, Philippe", 1),
                (PerformerKind::Ensemble, "Collegium Vocale Gent", 1),
                (PerformerKind::Soloist, "Eerens, Ilse", 1),
            ]
        );
        // and the roles they have on their other tracks too
        assert_eq!(dvorak[1].roles, performers[2].roles);

        // A performer's albums, whatever they do on them, with the tracks
        // that are theirs, and with a composer too
        let filter = super::TrackFilter {
            performer: Some("adrian chandler".to_string()),
            ..Default::default()
        };
        let mut albums: Vec<(String, (u32, u32))> =
            super::query_search_albums(&conn, &filter, &hidden)
                .unwrap()
                .into_iter()
                .collect();
        albums.sort();
        assert_eq!(
            albums,
            [("c".to_string(), (1, 1)), ("d".to_string(), (2, 2))]
        );
        let filter = super::TrackFilter {
            performer: Some("ilse eerens".to_string()),
            ..Default::default()
        };
        let albums = super::query_search_albums(&conn, &filter, &hidden).unwrap();
        assert_eq!(albums.get("a"), Some(&(2, 2)));
        assert_eq!(albums.len(), 1);
        let filter = super::TrackFilter {
            composer: Some("antonin dvorak".to_string()),
            ..filter
        };
        let albums = super::query_search_albums(&conn, &filter, &hidden).unwrap();
        assert_eq!(albums.get("a"), Some(&(1, 2)));
    }

    #[test]
    fn works_are_listed_with_their_recordings() {
        let conn = test_db();
        let dvorak = "\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n";
        let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
        let herreweghe = "\nc\tphilippe herreweghe\tPhilippe Herreweghe\tHerreweghe, Philippe\t\n";
        let eerens = "\ns\tilse eerens\tIlse Eerens\tEerens, Ilse\tsoprano vocals\n";
        // (path, album, year, disc, track, work, name, composers, order, performers, search)
        type Row<'a> = (
            &'a str,
            &'a str,
            Option<&'a str>,
            u32,
            u32,
            Option<&'a str>,
            &'a str,
            &'a str,
            Option<&'a str>,
            Option<&'a str>,
            &'a str,
        );
        let rows: &[Row] = &[
            // A Requiem, a track of something else, and a whole work
            (
                "/m/A/01",
                "a",
                Some("2015"),
                1,
                1,
                Some("req"),
                "Requiem, op. 89",
                dvorak,
                Some("1.1"),
                Some(herreweghe),
                "introitus",
            ),
            (
                "/m/A/02",
                "a",
                Some("2015"),
                1,
                2,
                Some("req"),
                "Requiem, op. 89",
                dvorak,
                Some("1.2"),
                Some(eerens),
                "graduale",
            ),
            (
                "/m/A/03",
                "a",
                Some("2015"),
                1,
                3,
                None,
                "",
                "",
                None,
                None,
                "interlude",
            ),
            (
                "/m/A/04",
                "a",
                Some("2015"),
                1,
                4,
                Some("sm"),
                "Stabat Mater",
                dvorak,
                None,
                None,
                "stabat",
            ),
            // Two recordings of a work one after the other, and a movement
            // in two tracks
            (
                "/m/B/01",
                "b",
                None,
                1,
                1,
                Some("gold"),
                "Goldberg Variations",
                bach,
                Some("1"),
                None,
                "aria",
            ),
            (
                "/m/B/02",
                "b",
                None,
                1,
                2,
                Some("gold"),
                "Goldberg Variations",
                bach,
                Some("2"),
                None,
                "variatio 1",
            ),
            (
                "/m/B/03",
                "b",
                None,
                1,
                3,
                Some("gold"),
                "Goldberg Variations",
                bach,
                Some("2"),
                None,
                "variatio 1 part 2",
            ),
            (
                "/m/B/04",
                "b",
                None,
                2,
                1,
                Some("gold"),
                "Goldberg Variations",
                bach,
                Some("1"),
                None,
                "aria",
            ),
            // Movements without their places
            (
                "/m/C/01",
                "c",
                Some("1995"),
                1,
                1,
                Some("req"),
                "Requiem v B-moll",
                dvorak,
                Some(""),
                Some(herreweghe),
                "kyrie",
            ),
            (
                "/m/C/02",
                "c",
                Some("1995"),
                1,
                2,
                Some("req"),
                "Requiem v B-moll",
                dvorak,
                Some(""),
                None,
                "agnus",
            ),
            // Out of the order of the work, and mixed with another
            (
                "/m/E/01",
                "e",
                None,
                1,
                1,
                Some("kdf"),
                "Die Kunst der Fuge",
                bach,
                Some("1"),
                None,
                "",
            ),
            (
                "/m/E/02",
                "e",
                None,
                1,
                2,
                Some("kdf"),
                "Die Kunst der Fuge",
                bach,
                Some("3"),
                None,
                "",
            ),
            (
                "/m/E/03",
                "e",
                None,
                1,
                3,
                Some("gold"),
                "Goldberg Variations",
                bach,
                Some("1"),
                None,
                "",
            ),
            (
                "/m/E/04",
                "e",
                None,
                1,
                4,
                Some("kdf"),
                "Die Kunst der Fuge",
                bach,
                Some("2"),
                None,
                "",
            ),
            // Linked to the whole work rather than to a movement of it
            (
                "/m/E/05",
                "e",
                None,
                1,
                5,
                Some("kdf"),
                "Die Kunst der Fuge",
                bach,
                None,
                None,
                "",
            ),
            // Numbers in order
            (
                "/m/D/01",
                "d",
                None,
                1,
                1,
                Some("s10"),
                "Symphony no. 10",
                dvorak,
                None,
                None,
                "",
            ),
            (
                "/m/D/02",
                "d",
                None,
                1,
                2,
                Some("s2"),
                "Symphony no. 2",
                dvorak,
                None,
                None,
                "",
            ),
            (
                "/m/Gone/01",
                "g",
                None,
                1,
                1,
                Some("req"),
                "Requiem, op. 89",
                dvorak,
                Some("1.1"),
                None,
                "gone",
            ),
        ];
        for (path, album, year, disc, track, key, name, composers, order, performers, search) in
            rows
        {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album, year,
                                     disc, track, search, work_key, work, work_composers,
                                     movement_order, performers)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    path.as_bytes(),
                    album,
                    year,
                    disc,
                    track,
                    search,
                    key,
                    key.map(|_| *name),
                    key.map(|_| *composers),
                    order,
                    performers
                ],
            )
            .unwrap();
        }
        let hidden = [PathBuf::from("/m/Gone")];
        let works = |filter: &super::TrackFilter| {
            super::query_works(&conn, filter, &hidden)
                .unwrap()
                .into_iter()
                .map(|work| (work.name, work.recordings))
                .collect::<Vec<_>>()
        };
        let all = super::TrackFilter::default();
        // By the composers' sort names, then by name, and the name most of
        // the tracks give
        assert_eq!(
            works(&all),
            [
                ("Die Kunst der Fuge".to_string(), 1),
                ("Goldberg Variations".to_string(), 3),
                ("Requiem v B-moll".to_string(), 2),
                ("Stabat Mater".to_string(), 1),
                ("Symphony no. 2".to_string(), 1),
                ("Symphony no. 10".to_string(), 1),
            ]
        );
        // The recordings with a track the search matches
        let graduale = super::TrackFilter {
            words: vec!["graduale".to_string()],
            ..Default::default()
        };
        assert_eq!(works(&graduale), [("Requiem v B-moll".to_string(), 1)]);
        let herreweghe_filter = super::TrackFilter {
            performer: Some("philippe herreweghe".to_string()),
            ..Default::default()
        };
        assert_eq!(
            works(&herreweghe_filter),
            [("Requiem v B-moll".to_string(), 2)]
        );

        // The recordings, each with all its tracks and the performers of
        // all of them, by year, those without one last
        let recordings = |key: &str, filter: &super::TrackFilter| {
            super::query_work_recordings(&conn, key, filter, &hidden)
                .unwrap()
                .into_iter()
                .map(|recording| {
                    let paths = recording
                        .paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    (recording.year, paths, recording.performers)
                })
                .collect::<Vec<_>>()
        };
        let strings = |paths: &[&str]| paths.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            recordings("req", &all),
            [
                (
                    Some("1995".to_string()),
                    strings(&["/m/C/01", "/m/C/02"]),
                    "Philippe Herreweghe".to_string()
                ),
                (
                    Some("2015".to_string()),
                    strings(&["/m/A/01", "/m/A/02"]),
                    "Philippe Herreweghe · Ilse Eerens".to_string()
                ),
            ]
        );
        assert_eq!(
            recordings("req", &graduale),
            [(
                Some("2015".to_string()),
                strings(&["/m/A/01", "/m/A/02"]),
                "Philippe Herreweghe · Ilse Eerens".to_string()
            )]
        );
        assert_eq!(
            recordings("gold", &all),
            [
                (
                    None,
                    strings(&["/m/B/01", "/m/B/02", "/m/B/03"]),
                    String::new()
                ),
                (None, strings(&["/m/B/04"]), String::new()),
                (None, strings(&["/m/E/03"]), String::new()),
            ]
        );
        // The other recordings of the work of a track
        let others = |path: &str| {
            super::query_track_work(&conn, Path::new(path), &hidden)
                .unwrap()
                .map(|(work, others)| (work.name, others))
        };
        assert_eq!(
            others("/m/B/04"),
            Some(("Goldberg Variations".to_string(), 2))
        );
        assert_eq!(others("/m/A/01"), Some(("Requiem v B-moll".to_string(), 1)));
        assert_eq!(others("/m/A/03"), None);
        assert_eq!(
            others("/m/Gone/01"),
            Some(("Requiem v B-moll".to_string(), 2))
        );
        assert_eq!(
            recordings("kdf", &all),
            [(
                None,
                strings(&["/m/E/01", "/m/E/02", "/m/E/04", "/m/E/05"]),
                String::new()
            )]
        );
    }

    #[test]
    fn albums_have_the_year_they_were_made_and_their_label() {
        use lofty::tag::{ItemKey, Tag, TagType};

        use super::ExtraComments;

        // A reissue goes by its first release, and a recording date first
        let path = Path::new("/music/Cello Suites/01.flac");
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::RecordingDate, "2007-03-01".to_string());
        tag.insert_text(ItemKey::OriginalReleaseDate, "1961".to_string());
        tag.insert_text(ItemKey::Label, "Hyperion".to_string());
        tag.insert_text(ItemKey::CatalogNumber, "CDA67100".to_string());
        let tags = super::TrackTags::from_tag(&tag, path, Some(path), &ExtraComments::default());
        assert_eq!(tags.year.as_deref(), Some("1961"));
        assert_eq!(tags.label.as_deref(), Some("Hyperion"));
        assert_eq!(tags.catalog_number.as_deref(), Some("CDA67100"));
        let extra = ExtraComments::with_recording_date("1960-05/1960-06");
        let tags = super::TrackTags::from_tag(&tag, path, Some(path), &extra);
        assert_eq!(tags.year.as_deref(), Some("1960"));

        // The album has the earliest year of its tracks, and its label
        let conn = test_db();
        for (path, year, label) in [
            ("/m/A/01", "1961", Some("Hyperion")),
            ("/m/A/02", "1960", None),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album, year,
                                     label, catalog_number)
                 VALUES (?1, ?1, 0, 0, 0, 0, 'a', 'Cello Suites', ?2, ?3, 'CDA67100')",
                rusqlite::params![path.as_bytes(), year, label],
            )
            .unwrap();
        }
        let albums = super::query_albums(&conn, &[]).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].year.as_deref(), Some("1960"));
        assert_eq!(albums[0].label.as_deref(), Some("Hyperion"));
        assert_eq!(albums[0].catalog_number.as_deref(), Some("CDA67100"));
    }

    #[test]
    fn an_album_without_an_album_artist_is_by_most_of_its_tracks() {
        let conn = test_db();
        let add = |album: &str, n: usize, artist: Option<&str>| {
            for i in 0..n {
                let path = format!("/m/{album}/{}-{i}", artist.unwrap_or("none"));
                conn.execute(
                    "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                         artist)
                     VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3)",
                    rusqlite::params![path.as_bytes(), album, artist],
                )
                .unwrap();
            }
        };
        // A track "feat." another artist doesn't make a compilation
        add("feat", 9, Some("A"));
        add("feat", 1, Some("A feat. B"));
        // Five artists, none on most of the tracks
        for artist in ["A", "B", "C", "D", "E"] {
            add("compilation", 2, Some(artist));
        }
        // Half isn't most
        add("half", 2, Some("A"));
        add("half", 2, Some("B"));
        // One artist, and tracks without one
        add("one", 1, Some("A"));
        add("one", 3, None);
        add("none", 2, None);

        let artists: std::collections::HashMap<String, String> = super::query_albums(&conn, &[])
            .unwrap()
            .into_iter()
            .map(|album| (album.id, album.artist))
            .collect();
        assert_eq!(artists["feat"], "A");
        assert_eq!(artists["compilation"], "Various Artists");
        assert_eq!(artists["half"], "Various Artists");
        assert_eq!(artists["one"], "A");
        assert_eq!(artists["none"], "Unknown artist");
    }
}
