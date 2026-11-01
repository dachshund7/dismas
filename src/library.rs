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
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
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
        Arrangement, AudioProperties, CoverCache, ExtraComments, FieldCode, FieldParts, FileTags,
        ListedPerformer, PerformerKind, ReadOptions, TrackCredits, field_values,
        normalize_key_text, normalize_search_text, plain_commas, read_file_tags, search_key,
        title_from_file_name, track_search_text, track_title,
    },
    catalogue::{self, CatalogueNumber},
    cover_image::CoverImage,
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::i18n,
    play_stats,
    search_query::{Candidate, Context, Mode, Needs, Query},
    utils,
};

// Bump when the schema or the way the tags are read changes, so that
// the whole library is read again
const SCHEMA_VERSION: i32 = 31;

// The paths of the tracks are kept as bytes, as they are not always valid
// UTF-8. The path of a track of a CUE sheet is under its sheet, which is
// the file the track comes from, with the audio files the sheet splits;
// the times and the size are those of the files. The tracks are made again
// for each version, but not the covers, nor the times their files were
// added, see `keep_added_times()`
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
        catalog_number TEXT,
        work_arrangement_of TEXT,
        work_arrangement TEXT,
        work_arrangement_of_id TEXT,
        work_part_ids TEXT,
        fields TEXT
    );
    CREATE INDEX tracks_album ON tracks (album_id);
    CREATE INDEX tracks_file ON tracks (file);
    CREATE INDEX tracks_work ON tracks (work_key);
    CREATE TABLE IF NOT EXISTS covers (
        album_id TEXT PRIMARY KEY,
        path TEXT
    );
    CREATE TABLE IF NOT EXISTS added_times (
        file BLOB PRIMARY KEY,
        added INTEGER NOT NULL
    );";

pub enum ScanEvent {
    // The number of files read so far, out of the files that changed
    Progress(usize, usize),
    // The number of album covers made so far, out of those to make, after
    // the files are read
    Covers(usize, usize),
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
    // The values the fields of the search look into, as in the queue, see
    // `FieldParts`
    fields: Option<String>,
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
    // For an arrangement, the title of the work it arranges, and what it
    // is, or "" if MusicBrainz doesn't say, see `Arrangement`
    arrangement_of: Option<String>,
    arrangement: Option<String>,
    // The MusicBrainz id of the work an arrangement arranges, if the plugin
    // gave it
    arrangement_of_id: Option<String>,
    // The MusicBrainz ids of the parts of the work it performs, at every
    // level below the work, on a line each, see `WorkLabels::part_ids`
    part_ids: Option<String>,
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
    let kept: Vec<bool> = keyed
        .iter()
        .enumerate()
        .map(|(i, (key, _))| {
            !keyed
                .iter()
                .enumerate()
                .any(|(j, (other, _))| key.starts_with(other) && (key != other || j < i))
        })
        .collect();
    keyed
        .into_iter()
        .zip(kept)
        .filter_map(|((_, value), kept)| kept.then_some(value))
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

// A query of the UI, which gives nothing when it fails, as being unable to
// do `what`
fn query_or_default<T: Default>(
    what: impl std::fmt::Display,
    query: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
) -> T {
    with_connection(query).unwrap_or_else(|e| {
        warn!("Unable to {what}: {e}");
        T::default()
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

    add_functions(&conn)?;

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
        keep_added_times(&tx);
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;

    Ok(conn)
}

// Keeps the time each file's tracks were added, before a new version of
// the schema drops them, until the scan has read the file again. Otherwise
// every track would be added at that scan: a new file counts as added
// when it was created, which without a birth time is when it was last
// written, as is a file that a sync tool replaced. A database too old to
// have these columns loses them, as before
fn keep_added_times(conn: &Connection) {
    if !matches!(has_table(conn, "tracks"), Ok(true)) {
        return;
    }
    // The WHERE keeps SQLite from taking ON CONFLICT for a join
    let kept = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS added_times (
             file BLOB PRIMARY KEY,
             added INTEGER NOT NULL
         );
         INSERT INTO added_times (file, added)
             SELECT file, MIN(added) FROM tracks WHERE true GROUP BY file
             ON CONFLICT (file) DO UPDATE SET added = MIN(added, excluded.added);",
    );
    if let Err(e) = kept {
        warn!("Unable to keep the times the tracks were added: {e}");
    }
}

// Whether the database has a table, which one made by an older version
// can lack
fn has_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )
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

// The path of bytes as `path_from_bytes()` gives it, without copying them
fn path_of_bytes(bytes: &[u8]) -> Cow<'_, Path> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Cow::Borrowed(Path::new(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        Cow::Owned(path_from_bytes(bytes.to_vec()))
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

// What the queue's search takes from the library for each track it has,
// by path: when it was added, and its album and work, for `added:`,
// `is:favorite-album` and `is:favorite-work`
#[derive(Clone, Debug, Default)]
pub struct TrackEntry {
    pub added: i64,
    pub album_id: Option<String>,
    pub work_key: Option<String>,
}

pub fn track_entries() -> HashMap<PathBuf, TrackEntry> {
    query_or_default("read the tracks for a search", |conn| {
        let mut stmt = conn.prepare("SELECT path, added, album_id, work_key FROM tracks")?;
        stmt.query_map([], |row| {
            let entry = TrackEntry {
                added: row.get(1)?,
                album_id: row.get(2)?,
                work_key: row.get(3)?,
            };
            Ok((path_from_bytes(row.get(0)?), entry))
        })?
        .collect()
    })
}

// The albums, sorted by artist, then year and title; the grid sorts them
// again in the order the user picked. The tracks in the `hidden` folders,
// which aren't there, are left out
pub fn albums(hidden: &[PathBuf]) -> Vec<AlbumInfo> {
    query_or_default("load the albums", |conn| query_albums(conn, hidden))
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
        let key = (key, glib::CollationKey::from(info.title.as_str()));
        Ok((key, info))
    })?;

    // Sorted by artist, then year and title, in the order of the
    // current locale
    let mut albums = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    albums.sort_by(|((a_artist, a_title), a), ((b_artist, b_title), b)| {
        (a_artist, &a.year, a_title).cmp(&(b_artist, &b.year, b_title))
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

// What the tracks taken from albums match: a search in the search syntax,
// matched as in the queue (`dismas_match()`), a composer and a performer, by
// their keys (`composer_key()`), a work, by its key (see `work_entry()`) and
// those of the arrangements listed under it, as the covers of a work's
// albums take its tracks, and the favorites. An empty one matches every
// track. Two filters made from the same search and picks are equal, as the
// time its relative dates go by is only taken when it's matched
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackFilter {
    // The search, unless it matches every track, see `TrackFilter::query()`
    pub query: Option<Arc<Query>>,
    // What the search needs from the window, if anything
    pub sources: Option<Arc<SearchSources>>,
    pub composer: Option<String>,
    pub performer: Option<String>,
    pub works: Vec<String>,
    pub favorites: Option<Favorites>,
}

// The favorites that the tracks have to be among. An empty list matches no
// track, as there are no favorites to show. Each is a value bound to the
// statement, and SQLite takes up to 32,766 of them, far more favorites
// than anyone marks; past that, the search fails and finds nothing
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Favorites {
    // The tracks of the favorite albums, by their ids
    Albums(Arc<[String]>),
    // The favorite tracks, by their paths in the library
    Tracks(Arc<[PathBuf]>),
}

impl Favorites {
    fn is_empty(&self) -> bool {
        match self {
            Self::Albums(ids) => ids.is_empty(),
            Self::Tracks(paths) => paths.is_empty(),
        }
    }
}

// What a search needs that the library doesn't keep
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchSources {
    // The music folders that came through the document portal, with their
    // paths outside the sandbox, which `path:` looks into
    pub shown_folders: Vec<(PathBuf, PathBuf)>,
}

// What `dismas_match()` matches a track with, bound to each statement, so
// that each has its own search, and a statement without one fails rather
// than matching with another's. The play stats and the favorites are read
// as the statement is made, in the thread it runs in
struct Matcher {
    // With its relative dates worked out at `context`, see `Query::at()`
    query: Query,
    context: Context,
    // From plays.db, by path rather than by URI
    heard: HashMap<PathBuf, play_stats::TrackStats>,
    favorites: HashMap<PathBuf, i64>,
    favorite_albums: BTreeSet<String>,
    favorite_works: HashSet<String>,
    shown_folders: Vec<(PathBuf, PathBuf)>,
}

// What a search reads from plays.db; tests give their own, and never read
// the user's
#[cfg(not(test))]
fn search_stats(needs: &Needs) -> play_stats::SearchStats {
    play_stats::search_stats(needs)
}

#[cfg(test)]
thread_local! {
    static TEST_SEARCH_STATS: RefCell<play_stats::SearchStats> = RefCell::default();
}

#[cfg(test)]
fn search_stats(_needs: &Needs) -> play_stats::SearchStats {
    TEST_SEARCH_STATS.with_borrow(Clone::clone)
}

impl Matcher {
    fn new(query: &Arc<Query>, sources: Option<&SearchSources>) -> Self {
        let needs = query.needs();
        let stats = search_stats(&needs);
        // As the library keeps the tracks, whose paths are those of their
        // URIs, even for the tracks of CUE sheets
        let by_path = |uri: &String| gio::File::for_uri(uri).path();
        let context = Context::now();
        Self {
            query: query.at(&context),
            context,
            heard: stats
                .heard
                .into_iter()
                .filter_map(|(uri, stats)| Some((by_path(&uri)?, stats)))
                .collect(),
            favorites: stats
                .favorites
                .into_iter()
                .filter_map(|(uri, marked)| Some((by_path(&uri)?, marked)))
                .collect(),
            favorite_albums: stats.favorite_albums,
            favorite_works: stats.favorite_works,
            shown_folders: sources
                .map(|sources| sources.shown_folders.clone())
                .unwrap_or_default(),
        }
    }
}

// The type of the pointer to a `Matcher` that statements are given
const MATCHER: &std::ffi::CStr = c"dismas-matcher";

// A value bound to a statement: a plain one, or the search it matches with
enum Param {
    Value(rusqlite::types::Value),
    Matcher(Rc<Matcher>),
}

impl rusqlite::ToSql for Param {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Self::Value(value) => value.to_sql(),
            Self::Matcher(matcher) => Ok(rusqlite::types::ToSqlOutput::from_rc(
                Rc::clone(matcher),
                MATCHER,
            )),
        }
    }
}

impl From<rusqlite::types::Value> for Param {
    fn from(value: rusqlite::types::Value) -> Self {
        Self::Value(value)
    }
}

// Adds `dismas_match(matcher, search, fields, path, added, album_id,
// work_key)` to a connection, which tells whether a track matches the
// search of the `Matcher` bound as its first argument, see
// `TrackFilter::conditions()`: the one evaluator of the search syntax, as
// the queue's search uses. A NULL for an argument the search doesn't need
// saves reading it. And `dismas_length(fields)`, a track's length in whole
// seconds from its `fields`, or NULL when it's unknown, which Play a
// Random Work weighs the works by, without a column of its own
fn add_functions(conn: &Connection) -> rusqlite::Result<()> {
    use rusqlite::{functions::FunctionFlags, types::ValueRef};

    conn.create_scalar_function(
        "dismas_length",
        1,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let ValueRef::Text(fields) = ctx.get_raw(0) else {
                return Ok(None);
            };
            Ok(std::str::from_utf8(fields).ok().and_then(|fields| {
                field_values(fields, &[FieldCode::Length])
                    .next()?
                    .parse::<i64>()
                    .ok()
            }))
        },
    )?;
    conn.create_scalar_function(
        "dismas_match",
        7,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let error = |e: &str| rusqlite::Error::UserFunctionError(e.into());
            // SAFETY: only `TrackFilter::conditions()` binds a pointer of this
            // type, which is an `Rc<Matcher>`
            let matcher = unsafe { ctx.get_pointer::<Matcher>(0, MATCHER) }
                .ok_or_else(|| error("dismas_match() without its search"))?;
            let text = |i| match ctx.get_raw(i) {
                ValueRef::Text(text) => std::str::from_utf8(text).ok(),
                _ => None,
            };
            let path = match ctx.get_raw(3) {
                ValueRef::Blob(path) => Some(path_of_bytes(path)),
                _ => None,
            };
            let favorited = path
                .as_ref()
                .and_then(|path| matcher.favorites.get(path.as_ref()).copied());
            let heard = path
                .as_ref()
                .and_then(|path| matcher.heard.get(path.as_ref()));
            let needs = matcher.query.needs();
            let shown = path.as_ref().map(|path| {
                matcher
                    .shown_folders
                    .iter()
                    .find_map(|(folder, shown)| Some(shown.join(path.strip_prefix(folder).ok()?)))
                    .map_or_else(|| Cow::Borrowed(path.as_ref()), Cow::Owned)
            });
            let candidate = Candidate {
                search_key: text(1).unwrap_or_default(),
                fields: text(2).unwrap_or_default(),
                path: shown.as_deref(),
                favorite: favorited.is_some(),
                missing: false,
                added: match ctx.get_raw(4) {
                    ValueRef::Integer(added) => Some(added),
                    _ => None,
                },
                played: heard.map(|heard| heard.last_heard),
                favorited,
                plays: needs
                    .stats
                    .then(|| heard.map_or(0, |heard| i64::from(heard.plays))),
                favorite_album: text(5).is_some_and(|id| matcher.favorite_albums.contains(id)),
                favorite_work: text(6).is_some_and(|key| matcher.favorite_works.contains(key)),
            };
            Ok(matcher.query.matches(&candidate, &matcher.context))
        },
    )
}

impl TrackFilter {
    // The search of a text, unless it matches every track
    pub fn query(text: &str) -> Option<Arc<Query>> {
        Some(Query::parse(text, Mode::Search))
            .filter(|query| !query.matches_all())
            .map(Arc::new)
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_none()
            && self.composer.is_none()
            && self.performer.is_none()
            && self.works.is_empty()
            && self.favorites.is_none()
    }

    // The SQL conditions on a track, each after " AND ", and their values;
    // a track without search text, composers, performers or a work matches
    // nothing
    fn conditions(&self) -> (String, Vec<Param>) {
        self.conditions_with(true)
    }

    // The conditions alone, without reading what the values need
    fn conditions_sql(&self) -> String {
        self.conditions_with(false).0
    }

    fn conditions_with(&self, with_values: bool) -> (String, Vec<Param>) {
        use rusqlite::types::Value;

        let mut sql = String::new();
        let mut values: Vec<Param> = Vec::new();
        if let Some(key) = &self.composer {
            sql.push_str(" AND instr(composers, ?) > 0");
            values.push(Value::Text(format!("\n{key}\t")).into());
        }
        if let Some(key) = &self.performer {
            sql.push_str(" AND instr(performers, ?) > 0");
            values.push(Value::Text(format!("\t{key}\t")).into());
        }
        if !self.works.is_empty() {
            sql.push_str(&format!(
                " AND work_key IN ({})",
                placeholders(self.works.len())
            ));
            values.extend(
                self.works
                    .iter()
                    .cloned()
                    .map(|key| Value::Text(key).into()),
            );
        }
        match &self.favorites {
            Some(favorites) if favorites.is_empty() => sql.push_str(" AND 0"),
            Some(Favorites::Albums(ids)) => {
                sql.push_str(&format!(" AND album_id IN ({})", placeholders(ids.len())));
                values.extend(ids.iter().cloned().map(|id| Value::Text(id).into()));
            }
            Some(Favorites::Tracks(paths)) => {
                sql.push_str(&format!(" AND path IN ({})", placeholders(paths.len())));
                values.extend(
                    paths
                        .iter()
                        .map(|path| Value::Blob(path_bytes(path).to_vec()).into()),
                );
            }
            None => {}
        }
        // Last, so that the conditions before it, which SQLite checks first,
        // leave out what they can
        if let Some(query) = &self.query {
            let needs = query.needs();
            let column = |needed: bool, column: &'static str| if needed { column } else { "NULL" };
            sql.push_str(&format!(
                " AND dismas_match(?, {}, {}, {}, {}, {}, {})",
                column(needs.search_key, "search"),
                column(needs.fields, "fields"),
                column(needs.path || needs.favorites || needs.stats, "path"),
                column(needs.added, "added"),
                column(needs.favorite_albums, "album_id"),
                column(needs.favorite_works, "work_key"),
            ));
            // Made only for the values, as it reads plays.db
            if with_values {
                values.push(Param::Matcher(Rc::new(Matcher::new(
                    query,
                    self.sources.as_deref(),
                ))));
            }
        }
        (sql, values)
    }

    // An SQL expression of whether a track matches, 1 or 0, for the values
    // of `conditions()`; `instr()` on a track without composers or
    // performers gives NULL, which only a WHERE takes as false
    fn matches_sql(&self) -> String {
        format!("(CASE WHEN 1{} THEN 1 ELSE 0 END)", self.conditions_sql())
    }
}

// The placeholders of `n` values in a list of SQL, e.g. "?, ?"
fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).join(", ")
}

// The values of a statement that has the filter's `matches_sql()` before
// the condition on the `hidden` folders, see `hidden_condition()`
fn filter_values(filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<Param> {
    use rusqlite::types::Value;

    let (_, values) = filter.conditions();
    let (_, hidden_params) = hidden_condition("file", hidden);
    values
        .into_iter()
        .chain(
            hidden_params
                .into_iter()
                .map(|path| Value::Blob(path).into()),
        )
        .collect()
}

// The albums with a track that the filter matches, like searching the
// queue, with the number of their tracks that it does and the number of all
// their tracks; the tracks in the `hidden` folders are left out, as in
// `albums()`
pub fn search_albums(filter: &TrackFilter, hidden: &[PathBuf]) -> HashMap<String, (u32, u32)> {
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
pub fn album_track_paths(hidden: &[PathBuf]) -> Vec<(String, Vec<PathBuf>)> {
    query_or_default("load the tracks of the albums", |conn| {
        query_album_track_paths(conn, hidden)
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
    // The number of albums, or works, with a track by the composer that
    // the filter matches, see `Counted`
    pub count: u32,
}

// What the lists of composers and performers count: the albums with a
// track that the filter matches, for the album grid, or the works, for
// the Works page, where the tracks that aren't part of a work count for
// nothing, so that picking someone listed always finds some
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counted {
    Albums,
    Works,
}

// Whether a track of the albums of the grid has a value in `column`, e.g.
// a composer, without the tracks in the `hidden` folders
fn query_tracks_have(
    conn: &Connection,
    column: &str,
    hidden: &[PathBuf],
) -> rusqlite::Result<bool> {
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM tracks
             WHERE album_id IS NOT NULL AND {column} IS NOT NULL{hidden_sql})"
        ),
        rusqlite::params_from_iter(hidden_params),
        |row| row.get(0),
    )
}

// The album, the album or the work it counts for, if the filter matches
// it (see `Counted`), and the lines of `column` (see `composer_entries()`
// and `performer_entries()`), of each track of the albums of the grid with
// some, without the tracks in the `hidden` folders
fn read_entries(
    conn: &Connection,
    column: &str,
    counted: Counted,
    filter: &TrackFilter,
    hidden: &[PathBuf],
    mut read: impl FnMut(&str, Option<&str>, &str),
) -> rusqlite::Result<()> {
    let counted = match counted {
        Counted::Albums => "album_id",
        Counted::Works => "work_key",
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, CASE WHEN {} = 1 THEN {counted} END, {column} FROM tracks
         WHERE album_id IS NOT NULL AND {column} IS NOT NULL{}",
        filter.matches_sql(),
        hidden_condition("file", hidden).0
    ))?;
    let mut rows = stmt.query(rusqlite::params_from_iter(filter_values(filter, hidden)))?;
    while let Some(row) = rows.next()? {
        read(
            row.get_ref(0)?.as_str()?,
            row.get_ref(1)?.as_str_or_null()?,
            row.get_ref(2)?.as_str()?,
        );
    }
    Ok(())
}

// Whether a track of the albums of the grid has a composer, without the
// tracks in the `hidden` folders
pub fn has_composers(hidden: &[PathBuf]) -> bool {
    query_or_default("look for composers", |conn| {
        query_tracks_have(conn, "composers", hidden)
    })
}

// The composers of the tracks of the albums of the grid that the filter
// matches, by their sort names, without the tracks in the `hidden` folders,
// with the number of the albums, or works, of those tracks
pub fn composers(hidden: &[PathBuf], filter: &TrackFilter, counted: Counted) -> Vec<ComposerInfo> {
    query_or_default("list the composers", |conn| {
        query_composers(conn, hidden, filter, counted)
    })
}

// A composer's names are the ones all their tracks give them the most,
// whatever the filter, so that they're the same in every list
fn query_composers(
    conn: &Connection,
    hidden: &[PathBuf],
    filter: &TrackFilter,
    counted: Counted,
) -> rusqlite::Result<Vec<ComposerInfo>> {
    // The albums or works of a composer that the filter matches, and how
    // often each of its names is given
    #[derive(Default)]
    struct Seen {
        counted: HashSet<String>,
        names: HashMap<String, usize>,
        sorts: HashMap<String, usize>,
    }

    let mut seen: HashMap<String, Seen> = HashMap::new();
    read_entries(
        conn,
        "composers",
        counted,
        filter,
        hidden,
        |_, counts_for, entries| {
            for line in entries.lines() {
                let mut fields = line.split('\t');
                let (Some(key), Some(name), Some(sort)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                let composer = entry_of(&mut seen, key);
                if let Some(counts_for) = counts_for {
                    insert_str(&mut composer.counted, counts_for);
                }
                count(&mut composer.names, name);
                if !sort.is_empty() {
                    count(&mut composer.sorts, sort);
                }
            }
        },
    )?;

    let mut composers: Vec<(glib::CollationKey, ComposerInfo)> = seen
        .into_iter()
        .filter(|(_, seen)| !seen.counted.is_empty())
        .map(|(key, seen)| {
            let name = most_common(seen.names);
            let tagged = most_common(seen.sorts);
            let sort = composer_sort(&name, &tagged);
            let label = if tagged.is_empty() {
                name.clone()
            } else {
                tagged
            };
            let collation = glib::CollationKey::from(sort.as_str());
            let info = ComposerInfo {
                key,
                search: normalize_search_text(&format!("{name} {sort}")),
                name,
                sort,
                label,
                count: seen.counted.len() as u32,
            };
            (collation, info)
        })
        .collect();
    composers.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.key.cmp(&b.1.key)));
    Ok(composers.into_iter().map(|(_, info)| info).collect())
}

// Of the values counted, the most common, and of those the first in
// alphabetical order
fn most_common<S: Ord + Into<String>>(counts: HashMap<S, usize>) -> String {
    most_counted(counts).map(Into::into).unwrap_or_default()
}

// The same, if anything was counted
fn most_counted<S: Ord>(counts: HashMap<S, usize>) -> Option<S> {
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(value, _)| value)
}

// The lists of composers and performers go through the values of every
// track, which are mostly ones seen already, so these copy a value only
// the first time

// Counts a value once more
fn count(counts: &mut HashMap<String, usize>, value: &str) {
    if let Some(n) = counts.get_mut(value) {
        *n += 1;
    } else {
        counts.insert(value.to_owned(), 1);
    }
}

// Adds a value to a set
fn insert_str(set: &mut HashSet<String>, value: &str) {
    if !set.contains(value) {
        set.insert(value.to_owned());
    }
}

// The value of a key, a new one the first time
fn entry_of<'a, V: Default>(map: &'a mut HashMap<String, V>, key: &str) -> &'a mut V {
    if !map.contains_key(key) {
        map.insert(key.to_owned(), V::default());
    }
    map.get_mut(key).unwrap()
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
    // The number of albums, or works, with a track they perform on that
    // the filter matches, see `Counted`
    pub count: u32,
}

// Whether a track of the albums of the grid has a conductor or performer,
// without the tracks in the `hidden` folders
pub fn has_performers(hidden: &[PathBuf]) -> bool {
    query_or_default("look for performers", |conn| {
        query_tracks_have(conn, "performers", hidden)
    })
}

// The conductors, groups and players of the tracks of the albums of the
// grid that the filter matches, in that order, each by their sort names,
// without the tracks in the `hidden` folders, with the number of the
// albums of those tracks
pub fn performers(
    hidden: &[PathBuf],
    filter: &TrackFilter,
    counted: Counted,
) -> Vec<PerformerInfo> {
    query_or_default("list the performers", |conn| {
        query_performers(conn, hidden, filter, counted)
    })
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
    counted: Counted,
) -> rusqlite::Result<Vec<PerformerInfo>> {
    // The albums or works of a performer that the filter matches, the
    // albums they have each kind on, and how often each of their names and
    // roles is given
    #[derive(Default)]
    struct Seen {
        counted: HashSet<String>,
        kinds: HashMap<PerformerKind, HashSet<String>>,
        names: HashMap<String, usize>,
        sorts: HashMap<String, usize>,
        roles: HashMap<String, usize>,
    }

    let mut seen: HashMap<String, Seen> = HashMap::new();
    read_entries(
        conn,
        "performers",
        counted,
        filter,
        hidden,
        |album, counts_for, entries| {
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
                let performer = entry_of(&mut seen, key);
                if let Some(counts_for) = counts_for {
                    insert_str(&mut performer.counted, counts_for);
                }
                let guessed = kind == PerformerKind::Ensemble && role.is_empty();
                if !guessed {
                    insert_str(performer.kinds.entry(kind).or_default(), album);
                }
                count(&mut performer.names, name);
                count(&mut performer.sorts, sort);
                if !role.is_empty() {
                    count(&mut performer.roles, role);
                }
            }
        },
    )?;

    let mut performers: Vec<(glib::CollationKey, PerformerInfo)> = seen
        .into_iter()
        .filter(|(_, seen)| !seen.counted.is_empty())
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
            let collation = glib::CollationKey::from(sort.as_str());
            let info = PerformerInfo {
                key,
                kind,
                search: normalize_search_text(&format!("{name} {sort} {}", roles.join(" "))),
                name,
                sort,
                roles,
                count: seen.counted.len() as u32,
            };
            (collation, info)
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
    // For an arrangement, the work it arranges, and what it is, if
    // MusicBrainz says
    pub arrangement: Option<Arrangement>,
    // The keys of the arrangements of it that are listed under it, rather
    // than on their own, see `listed_under()`
    pub arrangements: Vec<String>,
    pub composers: Vec<WorkComposer>,
    // The number of its recordings that the filter matches, those of its
    // arrangements included, see `recordings_of_works()`
    pub recordings: u32,
    // Whether it's a favorite, or one of its recordings is; they're in the
    // play stats, as they can't be read again from the music
    pub favorite: bool,
}

impl WorkInfo {
    // Its key and those of the arrangements listed under it, the key first,
    // which take its tracks from the albums, see `TrackFilter`
    pub fn keys(&self) -> Vec<String> {
        self.all_keys().cloned().collect()
    }

    fn all_keys(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.key).chain(&self.arrangements)
    }

    // Whether it's among the `favorites`, the keys of the works marked and
    // of those of the favorite recordings, see
    // `play_stats::favorite_work_keys()`: by its own key, or by that of an
    // arrangement listed under it
    pub fn is_among(&self, favorites: &HashSet<String>) -> bool {
        self.all_keys().any(|key| favorites.contains(key))
    }

    // The names of its composers, as a list
    pub fn composer_names(&self) -> String {
        self.composers
            .iter()
            .map(|composer| &composer.name)
            .join(", ")
    }

    // The keys of its composers, which tell the works of other composers
    // apart, see `composer_key()`
    pub fn composer_keys(&self) -> impl Iterator<Item = &str> {
        self.composers.iter().map(|composer| composer.key.as_str())
    }
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
    pub year: Option<String>,
    // The conductors and performers of its tracks, as the credits line of
    // a track names them, see `performers_line()`
    pub performers: String,
    pub paths: Vec<PathBuf>,
    // The key of the work of its first track, which it's found by, e.g. as
    // a favorite
    pub work_key: String,
    // What it is, if it's all of an arrangement listed under the work, see
    // `listed_under()`
    pub arrangement: Option<Arrangement>,
}

// A track of an album with works, as the recordings of the works are
// worked out from
struct WorkRow {
    album_id: String,
    path: PathBuf,
    work: String,
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

// Whether a track is in an album, without the tracks in the `hidden`
// folders; with only the folders left out hidden, it says whether the
// music folders that are missing or empty have albums
pub fn has_albums(hidden: &[PathBuf]) -> bool {
    query_or_default("look for albums", |conn| {
        query_tracks_have(conn, "album_id", hidden)
    })
}

// Whether a track of the albums of the grid is part of a work, without the
// tracks in the `hidden` folders
pub fn has_works(hidden: &[PathBuf]) -> bool {
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
pub fn works(filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<WorkInfo> {
    query_or_default("list the works", |conn| query_works(conn, filter, hidden))
}

fn query_works(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkInfo>> {
    let rows = query_work_rows(conn, filter, &[], hidden, Lengths::Skip)?;
    Ok(works_of_rows(&rows))
}

// A work the Works page lists, as Play a Random Work picks from them
#[derive(Debug)]
pub struct WorkCandidate {
    pub info: WorkInfo,
    // The recordings of it that the filter matches, its own and those of
    // the arrangements listed under it
    pub recordings: Vec<RecordingCandidate>,
    // The tracks of all its recordings, those the filter doesn't match
    // too, which tell when it was heard last
    pub paths: Vec<PathBuf>,
}

// A recording of a work that Play a Random Work can play
#[derive(Debug)]
pub struct RecordingCandidate {
    pub album_id: String,
    // The work of its first track, by which a favorite recording is kept,
    // see `play_stats::FavoriteRecording`
    pub work_key: String,
    // Its tracks, in the order of their album
    pub paths: Vec<PathBuf>,
    // Its length in whole seconds, from those of its tracks that are known,
    // as if the others were as long, or `None` when none is
    pub length: Option<u64>,
    // Whether it's a single movement of the work, in one track or several,
    // such as an aria of an opera on a recital
    pub single_movement: bool,
    // What it is, if it's all of an arrangement listed under the work, as
    // in `WorkRecording`
    pub arrangement: Option<Arrangement>,
}

// The works that `works()` lists for the filter, in its order, each with
// the recordings of it that the filter matches, as `work_recordings()`
// gives them with its keys; the tracks in the `hidden` folders are left
// out
pub fn work_recording_candidates(filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<WorkCandidate> {
    query_or_default("load the recordings of the works", |conn| {
        query_work_recording_candidates(conn, filter, hidden)
    })
}

fn query_work_recording_candidates(
    conn: &Connection,
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> rusqlite::Result<Vec<WorkCandidate>> {
    let rows = query_work_rows(conn, filter, &[], hidden, Lengths::Read)?;
    Ok(listed_works(&rows)
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
fn works_of_rows(rows: &[WorkRow]) -> Vec<WorkInfo> {
    listed_works(rows)
        .into_iter()
        .map(|listed| listed.info)
        .collect()
}

// The rows of a recording of a work, see `recordings_of_works()`, and
// whether the filter matches any of them
struct RecordingRows {
    rows: Vec<usize>,
    matching: bool,
}

// A work as the Works page lists it, with the recordings of it and of the
// arrangements listed under it, those the filter matches or not
struct ListedWork {
    info: WorkInfo,
    recordings: Vec<RecordingRows>,
}

// The works of the rows with a recording that the filter matches, sorted
// as in `works()`. The arrangements of a work among them are listed under
// it, see `listed_under()`, and their recordings count as its own
fn listed_works(rows: &[WorkRow]) -> Vec<ListedWork> {
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
    for recording in recordings_of_works(rows, |row| split_recording_work(row, &listed)) {
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
    pub key: &'a str,
    pub composers: &'a [WorkComposer],
    pub name: &'a str,
    // Its full title, whose catalogue numbers count when its name has none
    pub title: Option<&'a str>,
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
pub fn track_work(path: &Path, hidden: &[PathBuf]) -> Option<String> {
    query_or_default("look up the work of a track", |conn| {
        query_track_work(conn, path, hidden)
    })
}

fn query_track_work(
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
// by the year of their albums, those without one last, and then by the
// albums' titles. The tracks in the `hidden` folders are left out
pub fn work_recordings(
    keys: &[String],
    filter: &TrackFilter,
    hidden: &[PathBuf],
) -> Vec<WorkRecording> {
    query_or_default("list the recordings of a work", |conn| {
        query_work_recordings(conn, keys, filter, hidden)
    })
}

fn query_work_recordings(
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
    let mut recordings: Vec<(_, WorkRecording)> =
        recordings_of_works(&rows, |row| split_recording_work(row, &listed))
            .into_iter()
            .map(|recording| recording.into_iter().map(|i| &rows[i]).collect::<Vec<_>>())
            .filter(|rows| rows.iter().any(|row| row.matching))
            .map(|rows| {
                let album = rows.iter().find_map(|row| row.album.as_deref());
                let collation = glib::CollationKey::from(album.unwrap_or_default());
                let recording = WorkRecording {
                    album_id: rows[0].album_id.clone(),
                    year: rows
                        .iter()
                        .filter_map(|row| row.year.as_deref())
                        .min()
                        .map(str::to_owned),
                    performers: performers_line(
                        rows.iter().filter_map(|row| row.performers.as_deref()),
                    ),
                    paths: rows.iter().map(|row| row.path.clone()).collect(),
                    work_key: rows[0].work.clone(),
                    arrangement: recording_arrangement(&rows, |work| listed.contains_key(work)),
                };
                (collation, recording)
            })
            .collect();
    // By year, those without one last, then by album
    recordings.sort_by(|(a_album, a), (b_album, b)| {
        (a.year.is_none(), &a.year, a_album, &a.album_id).cmp(&(
            b.year.is_none(),
            &b.year,
            b_album,
            &b.album_id,
        ))
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
enum Lengths {
    Read,
    Skip,
}

// The tracks of the works, or of the works of `keys`, in the order of
// their albums; the other tracks of the albums don't split the recordings
// of a work, see `recordings_of_works()`
fn query_work_rows(
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
                work, work_title, work_collection, work_composers, album, year, performers,
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
            year: row.get(10)?,
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
// whole opera rather than to its scenes, so they make one recording,
// unless a movement comes again, as on an album with two recordings of the
// work. The parts of a movement in several tracks have its place, one
// after the other
fn recordings_of_works<'a>(
    rows: &'a [WorkRow],
    work_of: impl Fn(&'a WorkRow) -> &'a str,
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
        if !seen.insert(key) {
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

// The tracks the filter matches, as the Tracks page takes them from the
// albums, each with its title, if its tags give one; the tracks in the
// `hidden` folders are left out
pub fn matching_tracks(filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<(PathBuf, Option<String>)> {
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

// The files of the tracks of an album, in order; with a filter, only the
// tracks it matches, as in `search_albums()`. The tracks in the `hidden`
// folders are left out, as in `albums()`
pub fn album_tracks(album_id: &str, filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<PathBuf> {
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
pub fn album_tracks_and_discs(album_id: &str, hidden: &[PathBuf]) -> (Vec<PathBuf>, bool) {
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
pub enum Disc {
    Number(u32),
    Folder(PathBuf),
}

impl Disc {
    // The disc of a track with the disc number `number`, in the folder `dir`
    pub fn of(number: Option<u32>, dir: Option<&Path>) -> Self {
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
pub fn track_album(path: &Path, hidden: &[PathBuf]) -> Option<String> {
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

// Updates the database with the tracks in the music folders, and makes the
// covers of the albums for the grid at the sizes `thumbnail_sizes` gives;
// this blocks, so it should be called from a separate thread
pub fn scan(folders: &[MusicFolder], thumbnail_sizes: ThumbnailSizes, sender: &Sender<ScanEvent>) {
    let now = Instant::now();
    let roots = roots(folders);
    debug!("Library: reading {roots:?}");
    let mut trees = read_roots(&roots);
    let paths = trees
        .iter_mut()
        .flat_map(|tree| std::mem::take(&mut tree.files))
        .filter_map(|file| file.path())
        .collect();
    let mut result = with_connection(|conn| {
        let (mut result, stale_albums) = scan_with(conn, &roots, paths, sender)?;
        result.covers_changed = update_covers(
            conn,
            &stale_albums,
            &roots.unavailable,
            thumbnail_sizes,
            sender,
        )? > 0;
        Ok(result)
    })
    .unwrap_or_else(|e| {
        warn!("Unable to scan the library: {e}");
        ScanResult::default()
    });
    debug!("Library scan: {} ms", now.elapsed().as_millis());
    // Even when the database couldn't be read, the folders are watched
    result.watch = folders_to_watch(folders, &roots, &trees);

    let _ = sender.try_send(ScanEvent::Finished(result));
}

// What the scan reads in each music folder that's there, in their order,
// see `scan_reads()`; this blocks
fn read_roots(roots: &Roots) -> Vec<utils::FolderTree> {
    roots
        .available
        .iter()
        .map(|root| {
            utils::read_folder_tree(&gio::File::for_path(root), true, &|path| {
                scan_reads(path, roots)
            })
        })
        .collect()
}

// The folders of the library to watch for files that come, go or change:
// those in the music folders on this computer's disks, with the folders in
// them and those that symbolic links in them lead to, as the scan reads
// them, but not those that are hidden or left out, which it doesn't.
// Changes in a folder on a network share can be made from other
// computers, which aren't told about, and the folders that came through
// the document portal aren't watched either. Also the music folders and
// the folders that links lead to, for the drives mounted and unmounted,
// see `WatchList`. The `trees` are those of `read_roots()`. This blocks
fn folders_to_watch(
    folders: &[MusicFolder],
    roots: &Roots,
    trees: &[utils::FolderTree],
) -> WatchList {
    let mut list = WatchList {
        unavailable: !roots.unavailable.is_empty(),
        ..WatchList::default()
    };
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for (root, tree) in roots.available.iter().zip(trees) {
        list.available.push(canonical(root));
        let portal = folders
            .iter()
            .any(|folder| folder.path == *root && folder.host.is_some());
        if portal || is_remote(root) {
            debug!("Library: not watching {root:?}");
            list.unwatched = true;
            continue;
        }
        // A link to a drive that isn't mounted leads nowhere until it is
        list.unavailable |= tree.broken_links;
        let mut remote = Vec::new();
        for link in tree.links.iter().filter_map(gio::File::path) {
            list.available.push(canonical(&link));
            if is_remote(&link) {
                debug!("Library: not watching {link:?}");
                list.unwatched = true;
                remote.push(link);
            }
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
// folders: not a hidden one, nor a folder left out
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

// Updates the tracks from the files of the music folders, the `paths` of
// `read_roots()`, and returns the albums whose cover should be looked for
// again
fn scan_with(
    conn: &mut Connection,
    roots: &Roots,
    paths: Vec<PathBuf>,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<(ScanResult, HashSet<String>)> {
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
    // When the files were added whose tracks an older version of the
    // schema dropped, see `keep_added_times()`
    let kept: HashMap<Vec<u8>, i64> = conn
        .prepare("SELECT file, added FROM added_times")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    // Find the files that are new or changed since the last scan
    let mut present = HashSet::new();
    let mut changed = Vec::new();
    for path in paths {
        let audio: Vec<PathBuf> = if cue_sheet::is_cue_path(&path) {
            let audio: Vec<PathBuf> = claims
                .tracks_of(&path)
                .into_iter()
                .map(|(_, audio)| audio)
                .unique()
                .map(Path::to_path_buf)
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
                  label, catalog_number, work_arrangement_of, work_arrangement,
                  work_arrangement_of_id, work_part_ids, fields)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                         ?31)",
            )?;
            for source in chunk {
                let key = path_bytes(&source.path);
                for id in old_albums.query_map([key], |row| row.get::<_, String>(0))? {
                    stale_albums.insert(id?);
                }
                delete.execute([key])?;

                // A changed track keeps the time it was added, as do the
                // tracks of an audio file that a CUE sheet splits now, and
                // those of a file read again for a new version of the schema
                let was_known = known.contains_key(key);
                let added = std::iter::once(&source.path)
                    .chain(&source.audio)
                    .flat_map(|path| {
                        let file = path_bytes(path);
                        let known = known.get(file).map(|(_, added)| *added);
                        known.into_iter().chain(kept.get(file).copied())
                    })
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
                        work.and_then(|w| w.arrangement_of.as_ref()),
                        work.and_then(|w| w.arrangement.as_ref()),
                        work.and_then(|w| w.arrangement_of_id.as_ref()),
                        work.and_then(|w| w.part_ids.as_ref()),
                        tags.fields,
                    ])?;
                    n_changed += 1;
                    // A changed track might have new cover art; new tracks
                    // don't make the cover of their album stale, so that the
                    // covers are kept when the whole library is read again
                    if was_known {
                        stale_albums.extend(tags.album_id);
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
    let is_gone = |file: &&[u8]| {
        !present.contains(*file)
            && (roots.unavailable.is_empty() || {
                let path = path_of_bytes(file);
                !roots.unavailable.iter().any(|root| path.starts_with(root))
            })
    };
    let gone: Vec<&[u8]> = known.keys().map(Vec::as_slice).filter(is_gone).collect();
    let kept_gone: Vec<&[u8]> = kept.keys().map(Vec::as_slice).filter(is_gone).collect();
    if !gone.is_empty() || !kept.is_empty() {
        let tx = conn.transaction()?;
        if !gone.is_empty() {
            debug!("Library: {} files of tracks removed", gone.len());
            let mut old_albums = tx.prepare(
                "SELECT DISTINCT album_id FROM tracks WHERE file = ?1 AND album_id IS NOT NULL",
            )?;
            let mut delete = tx.prepare("DELETE FROM tracks WHERE file = ?1")?;
            for file in gone {
                for id in old_albums.query_map([file], |row| row.get::<_, String>(0))? {
                    stale_albums.insert(id?);
                }
                n_changed += delete.execute([file])?;
            }
        }
        if !kept.is_empty() {
            let mut forget = tx.prepare("DELETE FROM added_times WHERE file = ?1")?;
            for file in kept_gone {
                forget.execute([file])?;
            }
            // The tracks of the files read again have their times now
            tx.execute(
                "DELETE FROM added_times WHERE file IN (SELECT file FROM tracks)",
                [],
            )?;
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
    sizes: ThumbnailSizes,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<usize> {
    let dir = covers_dir();
    // Each album with its first track, and the thumbnail to make smaller,
    // with its new size, when it's only too large
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
            // Thumbnails at another size than the grid shows them, or in
            // the format of older versions, are made again, or smaller
            let update = cover.as_deref().map_or(ThumbnailUpdate::Keep, |p| {
                sizes.update(thumbnail_size(Path::new(p), &dir, &id))
            });
            // Albums without cover art have no thumbnail, so we only check
            // that the thumbnails we created are still there
            let made = cover
                .as_ref()
                .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
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
            if !has_row
                || update == ThumbnailUpdate::Remake
                || removed
                || cover_file_changed
                || stale_albums.contains(&id)
            {
                // A track that's gone since the scan would leave its album
                // without a cover for good
                if cue_sheet::real_path(&first_track).is_file() {
                    missing.push((id, first_track, None));
                }
            } else if let (ThumbnailUpdate::Shrink(size), Some(cover)) = (update, cover) {
                missing.push((id, first_track, Some((PathBuf::from(cover), size))));
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
        let total = missing.len();
        if total > 0 {
            let _ = sender.try_send(ScanEvent::Covers(0, total));
        }
        // Each cover is decoded in a sandboxed process, so making all of
        // them again, as when the grid shows them larger, takes a while: a
        // few are made at a time, which goes faster, as for the loader.
        // Making them smaller takes a fraction of that. They're written in
        // this thread, which has the transaction
        let (missing, next, dir) = (&missing, &AtomicUsize::new(0), &dir);
        let (made_sender, made_receiver) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            for _ in 0..THUMBNAIL_THREADS.min(total) {
                let made_sender = made_sender.clone();
                scope.spawn(move || {
                    while let Some((id, first_track, larger)) =
                        missing.get(next.fetch_add(1, Ordering::Relaxed))
                    {
                        let made = utils::guarded(
                            first_track.display(),
                            || match larger {
                                Some((thumbnail, size)) => {
                                    shrink_thumbnail(thumbnail, dir, id, *size).or_else(|| {
                                        create_thumbnail(first_track, dir, id, sizes.wanted)
                                    })
                                }
                                None => create_thumbnail(first_track, dir, id, sizes.wanted),
                            },
                            || None,
                        );
                        // The receiver is gone once writing failed
                        if made_sender.send((id, larger, made)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(made_sender);

            for (i, (id, larger, made)) in made_receiver.into_iter().enumerate() {
                let _ = sender.try_send(ScanEvent::Covers(i + 1, total));
                n_made += usize::from(made.is_some());
                // A thumbnail that couldn't be made smaller is kept as it is
                let cover = made
                    .as_deref()
                    .or_else(|| larger.as_ref().map(|(thumbnail, _)| thumbnail.as_path()))
                    .map(|path| path.to_string_lossy().to_string());
                upsert.execute(params![id, cover])?;
            }
            Ok::<_, rusqlite::Error>(())
        })?;
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

// How many of the album grid's thumbnails are made at a time. Decoding a
// cover mostly waits for its sandboxed loader, so a few at a time go
// faster, even on fewer cores, as for the loader's covers
const THUMBNAIL_THREADS: usize = 4;

// The sizes the album grid's thumbnails are made at, in pixels
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbnailSizes {
    // As large as the grid shows the covers, on the monitor with the
    // largest scale
    pub wanted: u32,
    // As large as the grid shows them at a scale of 2, the largest the
    // thumbnails are made for
    pub scale_2: u32,
}

// What a scan does with an album's thumbnail
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailUpdate {
    Keep,
    // Made smaller, at this size, from the thumbnail itself
    Shrink(u32),
    // Made again from the album's cover
    Remake,
}

impl ThumbnailSizes {
    // What to do with a thumbnail made at `made` pixels, or in the format
    // of older versions (`None`), which is made again, as is one smaller
    // than wanted. One made for a monitor at a scale of 2 is kept, as the
    // monitors can change from one start to the next, and docking a laptop
    // would otherwise make every thumbnail again. Another one larger than
    // wanted was made while the covers were zoomed in: reading a cover
    // smaller than its thumbnail scales it down each time, which takes
    // several times as long, so it's made smaller instead
    pub fn update(self, made: Option<u32>) -> ThumbnailUpdate {
        match made {
            Some(made) if made == self.wanted || made == self.scale_2 => ThumbnailUpdate::Keep,
            Some(made) if made > self.scale_2.max(self.wanted) => {
                ThumbnailUpdate::Shrink(self.scale_2.max(self.wanted))
            }
            Some(made) if made > self.wanted => ThumbnailUpdate::Shrink(self.wanted),
            _ => ThumbnailUpdate::Remake,
        }
    }
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
    save_thumbnail(&image, dir, album_id, size)
}

// Makes a smaller thumbnail of an album from a larger one, made while the
// covers were zoomed in, and returns its path. The larger one is read in the
// process (`CoverImage::open_thumbnail`), which takes a fraction of the time
// that decoding the cover again in the sandbox would
fn shrink_thumbnail(thumbnail: &Path, dir: &Path, album_id: &str, size: u32) -> Option<PathBuf> {
    let image = CoverImage::open_thumbnail(thumbnail)?.thumbnail(size)?;
    save_thumbnail(&image, dir, album_id, size)
}

fn save_thumbnail(image: &CoverImage, dir: &Path, album_id: &str, size: u32) -> Option<PathBuf> {
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
// the CUE sheet in the tags, with the properties and without the covers
fn read_tags_only(path: &Path) -> Option<FileTags> {
    read_file_tags(path, ReadOptions::SCAN)
}

// The tracks of an audio file: the file, or the tracks that the CUE sheet
// in its tags splits it into
fn read_tags(path: &Path) -> Vec<(PathBuf, TrackTags)> {
    let Some(tags) = read_tags_only(path) else {
        return vec![(path.to_path_buf(), TrackTags::default())];
    };
    let file = Audio::of(&tags);
    let FileTags { tag, extra, .. } = tags;

    let sheet = extra
        .cue_sheet
        .as_deref()
        .and_then(|text| SplitSheet::embedded(path, text));
    if let Some(sheet) = sheet {
        let album_tag = cue_sheet::album_tag(Some(&tag), &sheet.sheet);
        return sheet
            .tracks()
            .map(|(track, _)| cue_track_tags(&sheet, &album_tag, track, &file))
            .collect();
    }

    // A file without tags is in the album of its folder too
    let tags = TrackTags::from_tag(&tag, path, Some(path), &extra, file.whole());
    vec![(path.to_path_buf(), tags)]
}

// What the fields take from an audio file
#[derive(Default)]
struct Audio {
    format: Option<Cow<'static, str>>,
    properties: AudioProperties,
    duration: Duration,
}

impl Audio {
    fn of(tags: &FileTags) -> Self {
        Self {
            format: tags.format.clone(),
            properties: AudioProperties::of(&tags.properties),
            duration: tags.properties.duration(),
        }
    }

    // The format and the properties of the whole file
    fn whole(&self) -> (Option<&str>, Option<AudioProperties>) {
        (self.format.as_deref(), Some(self.properties))
    }

    // Those of a track of a CUE sheet, as long as its part of the file
    fn part(&self, track: &CueTrack) -> (Option<&str>, Option<AudioProperties>) {
        let length = track.length(self.duration);
        (
            self.format.as_deref(),
            Some(self.properties.with_length(length)),
        )
    }
}

// The tracks that a ".cue" file splits audio files into
fn read_sheet_tags(
    sheet: &SplitSheet,
    tracks: Vec<(&CueTrack, &Path)>,
) -> Vec<(PathBuf, TrackTags)> {
    let mut files = HashMap::new();
    tracks
        .into_iter()
        .map(|(track, audio)| {
            let (album_tag, file) = files.entry(audio).or_insert_with(|| {
                let file = read_tags_only(audio);
                let album_tag =
                    cue_sheet::album_tag(file.as_ref().map(|file| &file.tag), &sheet.sheet);
                (album_tag, file.as_ref().map(Audio::of).unwrap_or_default())
            });
            cue_track_tags(sheet, album_tag, track, file)
        })
        .collect()
}

fn cue_track_tags(
    sheet: &SplitSheet,
    album_tag: &Tag,
    track: &CueTrack,
    file: &Audio,
) -> (PathBuf, TrackTags) {
    let tag = cue_sheet::track_tag(album_tag, &sheet.sheet, track);
    // The album is in the folder of the sheet, and the items that lofty
    // doesn't read are about the whole audio file
    let extra = ExtraComments::default();
    let tags = TrackTags::from_tag(&tag, &sheet.path, None, &extra, file.part(track));
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

// The folder of the album that `dir` is a disc folder of, as Album is for
// Album/CD 1, where the album's cover and booklet are often left; `None`
// when it isn't a disc folder
pub fn disc_album_folder(dir: &Path) -> Option<&Path> {
    dir.parent()
        .filter(|_| dir.file_name().is_some_and(is_disc_folder))
}

// What stands for a track in the folder of its album, to be selected
// there, where `album_folder()` gives the folder itself: the disc folder
// it's in, as CD 1 is for Album/CD 1, or else its file. That's the sheet
// for a track of a sheet, the file the library keeps, rather than the
// audio file that the track menu selects, which would mean reading the
// sheet; the two are next to each other
pub fn album_folder_entry(path: &Path) -> &Path {
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
    fn from_tag(
        tag: &Tag,
        path: &Path,
        file: Option<&Path>,
        extra: &ExtraComments,
        (format, properties): (Option<&str>, Option<AudioProperties>),
    ) -> Self {
        let get = |key| tag.get_string(key).map(|s| s.trim().to_string());

        let album = get(ItemKey::AlbumTitle).filter(|s| !s.is_empty());
        let album_artist = tag
            .get_string(ItemKey::AlbumArtist)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let artist = tag.artist();
        let artist = artist.as_deref().map(str::trim).filter(|s| !s.is_empty());
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
            match (album.as_ref().and(album_artist), album_folder(path)) {
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
        let searched_title = title
            .as_deref()
            .map(Cow::Borrowed)
            .or_else(|| file.map(|file| title_from_file_name(file).into()));
        let search = search_key(
            Some(&artists),
            searched_title.as_deref(),
            &track_search_text(tag, &credits),
            folder_album.as_deref(),
        );
        let fields = FieldParts {
            tag,
            credits: &credits,
            title: searched_title.as_deref(),
            album: album.as_deref().or(folder_album.as_deref()),
            format,
            properties,
        }
        .encode();

        TrackTags {
            album_id: Some(album_id),
            album: album.or(folder_album),
            // After the id, which must not change
            album_artist: album_artist.map(plain_commas),
            artist: artist.map(plain_commas),
            artist_sort,
            year: credits.year(),
            disc: tag.disk(),
            track: tag.track(),
            title,
            search: Some(search),
            composers: composer_entries(&credits.composers, &credits.composer_sorts),
            work: work_entry(&credits, artist),
            performers: performer_entries(&credits.listed_performers()),
            label: credits.labels.into_iter().next(),
            catalog_number: credits.catalog_numbers.into_iter().next(),
            fields: Some(fields),
        }
    }
}

// The work a track performs, or a movement of. The work is told apart by
// its MusicBrainz id: the one the "Work Hierarchy" plugin picks, or else,
// for a track of a whole work, the work it's linked to, which for a
// movement would be the movement's. An arrangement of a single movement,
// which the tags make a movement of the work, is told apart by the
// arrangement it's linked to, as its name would make it one with the
// recordings of the work without MusicBrainz ids. Without one, works with
// the same name and composers are the same, as the queue's headers take
// them to be
fn work_entry(credits: &TrackCredits, artist: Option<&str>) -> Option<WorkEntry> {
    let name = credits.work.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    let is_movement = credits.is_movement();

    let composers = credited_work_composers(credits, artist);
    let key = credits
        .work_id
        .clone()
        .or_else(|| {
            credits
                .musicbrainz
                .work
                .as_ref()
                .filter(|_| !is_movement || credits.work_arrangement.is_some())
                .cloned()
        })
        .unwrap_or_else(|| named_work_key(name, &composers));
    let work_names: Vec<String> = composers.iter().map(|c| c.name.clone()).collect();
    let work_sorts: Vec<String> = composers.into_iter().map(|c| c.sort).collect();
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
        arrangement_of: credits
            .work_arrangement
            .as_ref()
            .map(|arrangement| arrangement.of.clone()),
        arrangement: credits
            .work_arrangement
            .as_ref()
            .map(|arrangement| arrangement.label.clone().unwrap_or_default()),
        arrangement_of_id: credits
            .work_arrangement
            .as_ref()
            .and_then(|arrangement| arrangement.of_id.clone()),
        part_ids: (!credits.work_part_ids.is_empty()).then(|| credits.work_part_ids.join("\n")),
    })
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

// The composers a track's work is listed under, as the queue's headers
// find them (`TrackData::work_composers()`): the work's own, or else the
// track's, or else the artist, whom classical releases often credit as
// the composer. Each has the sort name its tags give it, matched by name,
// when they give as many as there are composers, or else one worked out
// from the name. A track that isn't part of a work is given no artist, so
// that its composers are only the ones the tags name
pub(crate) fn credited_work_composers(
    credits: &TrackCredits,
    artist: Option<&str>,
) -> Vec<WorkComposer> {
    let names = &credits.composers;
    let sorts = &credits.composer_sorts;
    let sort_of = |composer: &str| {
        names
            .iter()
            .position(|name| composer_key(name) == composer_key(composer))
            .and_then(|i| sorts.get(i))
            .filter(|_| sorts.len() == names.len())
            .cloned()
            .unwrap_or_else(|| sort_name(composer))
    };
    let work_names = credits.work_composer_names();
    let named: Vec<&str> = if work_names.is_empty() {
        artist.into_iter().collect()
    } else {
        work_names.iter().map(String::as_str).collect()
    };
    named
        .into_iter()
        .map(|name| WorkComposer {
            key: composer_key(name),
            name: name.to_string(),
            sort: sort_of(name),
        })
        .collect()
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
            PerformerKind::Ensemble => Cow::Borrowed(performer.name.as_str()),
            PerformerKind::Conductor | PerformerKind::Soloist => sort_name(&performer.name).into(),
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
    normalize_key_text(name)
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
        MusicFolder, Roots, ThumbnailSizes, ThumbnailUpdate, is_mbid, path_bytes, path_from_bytes,
        shrink_thumbnail, thumbnail_name, thumbnail_size,
    };

    fn roots_of(paths: &[&Path]) -> Roots {
        let folders: Vec<MusicFolder> = paths
            .iter()
            .map(|path| MusicFolder::new(path.to_path_buf()))
            .collect();
        super::roots(&folders)
    }

    // Reads the music folders, as `scan()` does
    fn scan_roots(
        conn: &mut rusqlite::Connection,
        roots: &Roots,
        sender: &async_channel::Sender<super::ScanEvent>,
    ) -> rusqlite::Result<(super::ScanResult, std::collections::HashSet<String>)> {
        use gtk::gio::prelude::FileExt;

        let paths = super::read_roots(roots)
            .into_iter()
            .flat_map(|tree| tree.files)
            .filter_map(|file| file.path())
            .collect();
        super::scan_with(conn, roots, paths, sender)
    }

    fn watch_list(folders: &[MusicFolder]) -> super::WatchList {
        let roots = super::roots(folders);
        super::folders_to_watch(folders, &roots, &super::read_roots(&roots))
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
        super::add_functions(&conn).unwrap();
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
        let tags = super::TrackTags::from_tag(
            &tag,
            path,
            Some(path),
            &super::ExtraComments::default(),
            (None, None),
        );
        assert_eq!(tags.album.as_deref(), Some("Some Folder"));
        let search = tags.search.unwrap();
        assert!(search.contains("intro"));
        assert!(search.contains("some folder"));

        tag.insert_text(ItemKey::AlbumTitle, "Album".to_string());
        let tags = super::TrackTags::from_tag(
            &tag,
            path,
            Some(path),
            &super::ExtraComments::default(),
            (None, None),
        );
        assert_eq!(tags.title, None);
        let search = tags.search.unwrap();
        assert!(search.contains("intro"));
        assert!(!search.contains("some folder"));

        tag.insert_text(ItemKey::TrackTitle, "Overture".to_string());
        let tags = super::TrackTags::from_tag(
            &tag,
            path,
            Some(path),
            &super::ExtraComments::default(),
            (None, None),
        );
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
            super::TrackTags::from_tag(&tag, path, Some(path), extra, (None, None))
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
        assert_eq!(movement.part_ids.as_deref(), Some("s\nd"));
    }

    #[test]
    fn a_whole_work_in_a_track_is_kept_as_an_arrangement() {
        use lofty::tag::{ItemKey, Tag, TagType};

        use super::ExtraComments;

        // A prelude for the lute played on the harpsichord, which MusicBrainz
        // links to the catch-all for its arrangements, whole in one track
        const CATCH_ALL: &str = "18047db5-0000-0000-0000-000000000000";
        const ORIGINAL: &str = "d53cc638-0000-0000-0000-000000000000";
        let path = Path::new("/music/Keyboard/12.flac");
        let prelude = "Prelude in C minor, BWV 999";
        let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0,
            "path": [{ "id": CATCH_ALL, "title": prelude, "name": prelude,
                       "disambiguation": "catch-all for arrangements",
                       "arrangement_of": { "id": ORIGINAL, "title": prelude } }] }] });
        let extra = ExtraComments::with_work_hierarchy(&hierarchy.to_string());
        let work = |work_id: &str| {
            let mut tag = Tag::new(TagType::VorbisComments);
            tag.insert_text(ItemKey::Work, prelude.to_string());
            tag.insert_text(ItemKey::MusicBrainzWorkId, work_id.to_string());
            super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None))
                .work
                .unwrap()
        };

        let arrangement = work(CATCH_ALL);
        assert_eq!(arrangement.key, CATCH_ALL);
        assert_eq!(arrangement.arrangement_of.as_deref(), Some(prelude));
        assert_eq!(arrangement.arrangement.as_deref(), Some(""));
        assert_eq!(arrangement.arrangement_of_id.as_deref(), Some(ORIGINAL));
        assert_eq!(arrangement.part_ids, None);
        // Not when the tags name another work than the plugin
        assert_eq!(work(ORIGINAL).arrangement_of, None);
    }

    #[test]
    fn an_arranged_movement_is_kept_by_its_own_id() {
        use lofty::tag::{ItemKey, Tag, TagType};

        use super::ExtraComments;

        // A song of a cycle arranged for a piano trio, which MusicBrainz
        // links to the catch-all for the song's arrangements, whose title
        // the plugin splits into the work and movement tags
        const CATCH_ALL: &str = "6d1b4a3e-0000-0000-0000-000000000000";
        const SONG: &str = "c0f3a2b1-0000-0000-0000-000000000000";
        let path = Path::new("/music/Trios/13.flac");
        let title = "Schwanengesang, D. 957: IV. Ständchen";
        let tag_of = |arrangement_of: serde_json::Value| {
            let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0,
                "path": [{ "id": CATCH_ALL, "title": title, "name": title,
                           "disambiguation": "catch-all for arrangements",
                           "arrangement_of": arrangement_of }] }] });
            let extra = ExtraComments::with_work_hierarchy(&hierarchy.to_string());
            let mut tag = Tag::new(TagType::VorbisComments);
            tag.insert_text(ItemKey::Work, "Schwanengesang, D. 957".to_string());
            tag.insert_text(ItemKey::Movement, "Ständchen".to_string());
            tag.insert_text(ItemKey::MovementNumber, "4".to_string());
            tag.insert_text(ItemKey::Composer, "Franz Schubert".to_string());
            tag.insert_text(ItemKey::MusicBrainzWorkId, CATCH_ALL.to_string());
            super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None))
                .work
                .unwrap()
        };

        let arranged = tag_of(serde_json::json!({ "id": SONG, "title": title }));
        assert_eq!(arranged.key, CATCH_ALL);
        assert_eq!(arranged.name, "Schwanengesang, D. 957");
        assert_eq!(arranged.order.as_deref(), Some("4"));
        assert_eq!(arranged.arrangement_of.as_deref(), Some(title));
        assert_eq!(arranged.arrangement.as_deref(), Some(""));
        assert_eq!(arranged.arrangement_of_id.as_deref(), Some(SONG));

        // A movement that MusicBrainz doesn't link to its work, but that
        // arranges nothing, is still one with the work of the same name
        let movement = tag_of(serde_json::Value::Null);
        assert_eq!(movement.arrangement_of, None);
        assert!(
            movement.key.starts_with("t:schwanengesang"),
            "{}",
            movement.key
        );
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
        let search = super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None))
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
    fn album_folder_entry_is_the_disc_folder_or_the_file() {
        use super::album_folder_entry;

        // A track is taken for a sheet's only while the sheet is there
        let dir = test_dir("album-folder-entry");
        std::fs::create_dir(dir.join("CD 2")).unwrap();
        for name in ["Album.cue", "Album.flac", "CD 2/Album.cue"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }

        for (track, entry) in [
            ("01.flac", "01.flac"),
            ("CD 1/01.flac", "CD 1"),
            ("Album.cue/track03", "Album.cue"),
            ("Album.flac/track03", "Album.flac"),
            ("CD 2/Album.cue/track01", "CD 2"),
        ] {
            assert_eq!(
                album_folder_entry(&dir.join(track)),
                dir.join(entry),
                "{track}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tracks_without_an_album_are_grouped_by_folder() {
        use super::query_albums;
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
        scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();

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
            let query = super::TrackFilter::query(&words.join(" "));
            let hidden = [PathBuf::from("/music/unmounted")];
            let mut albums: Vec<(String, (u32, u32))> = super::query_search_albums(
                &conn,
                &super::TrackFilter {
                    query,
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

    // The thumbnails follow the zoom: made again when the covers are zoomed
    // in, made smaller when they come back, and kept when they were made for
    // a monitor at a scale of 2
    #[test]
    fn thumbnails_follow_the_zoom() {
        use ThumbnailUpdate::{Keep, Remake, Shrink};

        // At the normal size: 240 pixels at a scale of 1, and 480 at 2
        let scale_1 = ThumbnailSizes {
            wanted: 240,
            scale_2: 480,
        };
        let scale_2 = ThumbnailSizes {
            wanted: 480,
            scale_2: 480,
        };
        assert_eq!(scale_1.update(Some(240)), Keep);
        assert_eq!(scale_2.update(Some(480)), Keep);
        // Made while a monitor at a scale of 2 was connected
        assert_eq!(scale_1.update(Some(480)), Keep);
        // Made while the covers were zoomed in
        assert_eq!(scale_1.update(Some(320)), Shrink(240));
        assert_eq!(scale_1.update(Some(640)), Shrink(480));
        assert_eq!(scale_2.update(Some(640)), Shrink(480));
        // Too small, or in the format of older versions
        assert_eq!(scale_2.update(Some(240)), Remake);
        assert_eq!(scale_2.update(None), Remake);

        // Zoomed in, at a scale of 2
        let zoomed = ThumbnailSizes {
            wanted: 640,
            scale_2: 640,
        };
        assert_eq!(zoomed.update(Some(480)), Remake);
        assert_eq!(zoomed.update(Some(640)), Keep);
    }

    // A thumbnail made smaller keeps its shape, and its format: a JPEG, or a
    // PNG for one with transparency
    #[test]
    fn thumbnails_are_made_smaller_from_larger_ones() {
        let dir = gtk::glib::mkdtemp("dismas-shrink-XXXXXX").unwrap();
        let jpeg = dir.join("album-640.jpg");
        image::RgbImage::from_pixel(640, 320, image::Rgb([200, 100, 50]))
            .save(&jpeg)
            .unwrap();
        let png = dir.join("other-640-alpha.png");
        image::RgbaImage::from_pixel(640, 640, image::Rgba([200, 100, 50, 128]))
            .save(&png)
            .unwrap();

        let made = shrink_thumbnail(&jpeg, &dir, "album", 480).unwrap();
        assert_eq!(made, dir.join("album-480.jpg"));
        assert_eq!(image::image_dimensions(&made).unwrap(), (480, 240));

        let made = shrink_thumbnail(&png, &dir, "other", 480).unwrap();
        assert_eq!(made, dir.join("other-480-alpha.png"));
        assert_eq!(image::image_dimensions(&made).unwrap(), (480, 480));

        assert!(shrink_thumbnail(&dir.join("missing.jpg"), &dir, "gone", 480).is_none());
        for name in [
            "album-640.jpg",
            "album-480.jpg",
            "other-640-alpha.png",
            "other-480-alpha.png",
        ] {
            std::fs::remove_file(dir.join(name)).unwrap();
        }
        std::fs::remove_dir(&dir).unwrap();
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

        let (result, _) = scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
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
        let (result, _) = scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
        assert_eq!(result.n_changed, 0);

        // Without the sheet, the file is a track again, added when its
        // tracks were
        std::fs::remove_file(&cue).unwrap();
        let (result, _) = scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
        assert_eq!(result.n_changed, 3);
        let found = tracks(&conn, &music);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "Album/Album.wav");
        assert_eq!(found[0].file, "Album/Album.wav");
        assert_eq!(found[0].added, added);

        std::fs::remove_dir_all(&music).unwrap();
    }

    #[test]
    fn a_new_version_of_the_schema_keeps_when_the_tracks_were_added() {
        use crate::utils::write_test_wav;

        let music = test_dir("added-music");
        let away = test_dir("added-away");
        write_test_wav(&music.join("Kept.wav"), 1);
        write_test_wav(&music.join("Gone.wav"), 1);
        write_test_wav(&away.join("Away.wav"), 1);
        let dir = test_dir("added-db");
        let path = dir.join("library.db");
        let (sender, _receiver) = async_channel::unbounded();
        let kept = |conn: &rusqlite::Connection| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM added_times", [], |row| row.get(0))
                .unwrap()
        };

        let mut conn = super::open_at(&path).unwrap();
        scan_roots(&mut conn, &roots_of(&[&music, &away]), &sender).unwrap();
        // As if they had been added long ago, and an older version had made
        // the database
        conn.execute("UPDATE tracks SET added = 100", []).unwrap();
        conn.pragma_update(None, "user_version", super::SCHEMA_VERSION - 1)
            .unwrap();
        drop(conn);

        let mut conn = super::open_at(&path).unwrap();
        assert!(tracks(&conn, &music).is_empty());
        assert_eq!(kept(&conn), 3);
        // A file that's gone is forgotten, and a folder that isn't there
        // keeps its times until it's back
        std::fs::remove_file(music.join("Gone.wav")).unwrap();
        write_test_wav(&music.join("New.wav"), 1);
        std::fs::remove_dir_all(&away).unwrap();
        scan_roots(&mut conn, &roots_of(&[&music, &away]), &sender).unwrap();
        let found = tracks(&conn, &music);
        let added: Vec<_> = found
            .iter()
            .map(|row| (row.path.as_str(), row.added))
            .collect();
        assert_eq!(added.len(), 2);
        assert_eq!(added[0], ("Kept.wav", 100));
        assert_eq!(added[1].0, "New.wav");
        assert!(added[1].1 > 100);
        assert_eq!(kept(&conn), 1);

        std::fs::create_dir_all(&away).unwrap();
        write_test_wav(&away.join("Away.wav"), 1);
        scan_roots(&mut conn, &roots_of(&[&music, &away]), &sender).unwrap();
        let back: i64 = conn
            .query_row(
                "SELECT added FROM tracks WHERE path = ?1",
                [path_bytes(&away.join("Away.wav"))],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(back, 100);
        assert_eq!(kept(&conn), 0);

        drop(conn);
        for dir in [&music, &away, &dir] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    // Tracks to search, in a folder made for `name`: one tagged as Picard
    // tags it, with a "Work Hierarchy" tag, one without tags in a disc
    // folder, and a CUE sheet of two tracks, the last of which runs to the end
    // of its file; with the files, as they'd be opened
    fn tagged_test_files(name: &str) -> (PathBuf, Vec<PathBuf>) {
        use std::borrow::Cow;

        use lofty::{
            TextEncoding,
            id3::v2::{Frame, FrameId, Id3v2Tag, KeyValueFrame, TextInformationFrame},
        };

        use crate::utils::{write_test_id3v2, write_test_wav};

        fn text(id: &'static str, value: &str) -> Frame<'static> {
            Frame::Text(TextInformationFrame::new(
                FrameId::Valid(Cow::Borrowed(id)),
                TextEncoding::UTF8,
                value.to_string(),
            ))
        }

        let music = test_dir(name);
        let album = music.join("Brandenburg Concertos");
        let disc = music.join("Untagged/CD 2");
        let sheet = music.join("Sheet");
        for dir in [&album, &disc, &sheet] {
            std::fs::create_dir_all(dir).unwrap();
        }

        // Tagged as MusicBrainz Picard tags it, with a "Work Hierarchy" tag
        let tagged = album.join("01 Allegro.wav");
        write_test_wav(&tagged, 2);
        let mut id3v2 = Id3v2Tag::new();
        for (id, value) in [
            (
                "TIT2",
                "Brandenburg Concerto No. 2 in F major, BWV 1047: I. Allegro",
            ),
            ("TPE1", "Musica Antiqua Köln, Reinhard Goebel"),
            ("TPE2", "Johann Sebastian Bach; Musica Antiqua Köln"),
            ("TALB", "Brandenburgische Konzerte"),
            ("TCOM", "Johann Sebastian Bach"),
            ("TPE3", "Reinhard Goebel"),
            ("TCON", "Baroque"),
            ("TIT1", "Concertos"),
            ("TPOS", "1/2"),
            ("TRCK", "1/6"),
            ("TDRC", "1986"),
            ("TPUB", "Erato"),
        ] {
            id3v2.insert(text(id, value));
        }
        id3v2.insert(Frame::KeyValue(KeyValueFrame::new(
            FrameId::Valid(Cow::Borrowed("TMCL")),
            TextEncoding::UTF8,
            vec![("violin".into(), "Reinhard Goebel".into())],
        )));
        let hierarchy = serde_json::json!({ "version": 1, "segments": [{
            "work_index": 0, "attributes": ["live"], "path": [
                { "id": "1a0d0fe3-55e6-4d0e-b4b3-6d1a8e6c3f9b",
                  "title": "Brandenburg Concerto no. 2 in F major, BWV 1047",
                  "name": "Brandenburg Concerto no. 2 in F major, BWV 1047",
                  "type": "Concerto" },
                { "id": "2b1e1fe4-66f7-4e1f-c5c4-7e2b9f7d4a0c",
                  "title": "Brandenburg Concerto no. 2 in F major, BWV 1047: I. Allegro",
                  "name": "Allegro", "number": 1, "position": 1 },
            ] }] });
        let _ = id3v2.insert_user_text("WORK_HIERARCHY".to_string(), hierarchy.to_string());
        write_test_id3v2(&tagged, &id3v2, false);

        // Without tags, named by its file and its folder
        write_test_wav(&disc.join("03 - Gigue.wav"), 1);

        // A CUE sheet, whose last track runs to the end of the file
        write_test_wav(&sheet.join("Recital.wav"), 10);
        let cue = sheet.join("Recital.cue");
        std::fs::write(
            &cue,
            "PERFORMER \"Rosalyn Tureck\"\nTITLE \"Recital\"\nFILE \"Recital.wav\" WAVE\n\
             TRACK 01 AUDIO\nTITLE \"Partita no. 1, BWV 825: Praeludium\"\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 00:04:00\n",
        )
        .unwrap();

        let files = vec![
            tagged,
            disc.join("03 - Gigue.wav"),
            cue,
            sheet.join("Recital.wav"),
        ];
        (music, files)
    }

    #[test]
    fn the_queue_and_the_library_encode_the_same_fields() {
        use gtk::gio::prelude::FileExt;

        let (music, files) = tagged_test_files("fields");
        let [tagged, gigue, cue, _] = <[PathBuf; 4]>::try_from(files.clone()).unwrap();

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
        let library: std::collections::BTreeMap<PathBuf, String> = conn
            .prepare("SELECT path, fields FROM tracks")
            .unwrap()
            .query_map([], |row| Ok((path_from_bytes(row.get(0)?), row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        let files = files.iter().map(gtk::gio::File::for_path).collect();
        let receiver = crate::audio::load_tracks(files, false);
        let queue: std::collections::BTreeMap<PathBuf, String> =
            std::iter::from_fn(|| receiver.recv_blocking().ok())
                .flatten()
                .map(|data| (data.file().path().unwrap(), data.fields().to_string()))
                .collect();

        assert_eq!(library.len(), 4);
        assert_eq!(queue, library);
        // What they have in common is the point; a few lines show that
        // they're the fields a search needs
        let fields = &library[&tagged];
        for line in [
            "fm\tconcerto",
            "ca\tbwv 1047",
            "is\tlive",
            "is\tmovement",
            "ro\tviolin",
            "gr\tconcertos",
        ] {
            assert!(fields.contains(&format!("{line}\n")), "{line} in {fields}");
        }
        let untagged = &library[&gigue];
        assert!(untagged.contains("ti\tgigue\n"), "{untagged}");
        assert!(untagged.contains("al\tuntagged\n"), "{untagged}");
        let second = &library[&cue.join("track02")];
        assert!(second.contains("ln\t6\n"), "{second}");

        std::fs::remove_dir_all(&music).unwrap();
    }

    // The tracks the library finds with a filter, by their paths
    fn library_finds(conn: &rusqlite::Connection, filter: &super::TrackFilter) -> Vec<PathBuf> {
        let (conditions, values) = filter.conditions();
        let mut paths: Vec<PathBuf> = conn
            .prepare(&format!("SELECT path FROM tracks WHERE 1{conditions}"))
            .unwrap()
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok(path_from_bytes(row.get(0)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        paths.sort();
        paths
    }

    #[test]
    fn the_queue_and_the_library_find_the_same_tracks() {
        use std::collections::HashMap;

        use gtk::gio::prelude::FileExt;

        use crate::search_query::Context;

        let (music, files) = tagged_test_files("finds");
        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
        let receiver =
            crate::audio::load_tracks(files.iter().map(gtk::gio::File::for_path).collect(), false);
        let tracks: Vec<crate::audio::Track> = std::iter::from_fn(|| receiver.recv_blocking().ok())
            .flatten()
            .map(crate::audio::Track::new)
            .collect();
        assert_eq!(tracks.len(), 4);

        // A favorite, played 4 times, and the other tracks never heard
        let uri = gtk::gio::File::for_path(&files[0]).uri().to_string();
        let stats = crate::play_stats::SearchStats {
            heard: [(
                uri.clone(),
                crate::play_stats::TrackStats {
                    plays: 4,
                    last_heard: 1_700_000_000,
                },
            )]
            .into(),
            favorites: [(uri, 1_700_000_100)].into(),
            ..Default::default()
        };
        super::TEST_SEARCH_STATS.set(stats.clone());
        let sources = crate::search::QueueSources {
            stats,
            tracks: HashMap::new(),
        };
        let context = Context::now();

        for (text, found) in [
            ("allegro", 1),
            ("composer:bach", 1),
            ("-composer:bach", 3),
            ("composer:(bach OR handel)", 1),
            ("people:goebel", 1),
            ("year:<1990", 1),
            ("released:1986", 1),
            ("is:live", 1),
            ("-is:live", 3),
            ("has:work", 1),
            ("form:concerto", 1),
            ("catalogue:bwv825", 1),
            ("catalogue:bwv", 2),
            ("length:>5", 1),
            ("path:sheet", 2),
            ("path:untagged/cd", 1),
            ("\"rosalyn tureck\"", 2),
            ("bach OR tureck", 3),
            ("is:favorite", 1),
            ("-is:favorite", 3),
            ("favorited:2023", 1),
            ("plays:>3", 1),
            ("plays:0", 3),
            ("played:<2024", 1),
            ("has:played", 1),
            ("-has:played", 3),
            ("format:wav", 4),
        ] {
            let query = super::TrackFilter::query(text).unwrap();
            let filter = super::TrackFilter {
                query: Some(query.clone()),
                ..Default::default()
            };
            let library = library_finds(&conn, &filter);
            let mut queue: Vec<PathBuf> = tracks
                .iter()
                .filter(|track| track.matches_query(&query, &context, Some(&sources)))
                .map(|track| track.file().path().unwrap())
                .collect();
            queue.sort();
            assert_eq!(queue, library, "{text}");
            assert_eq!(library.len(), found, "{text}: {library:?}");
        }

        std::fs::remove_dir_all(&music).unwrap();
    }

    #[test]
    fn a_statement_without_its_search_fails() {
        // Rather than matching with another statement's search
        let conn = test_db();
        let result: rusqlite::Result<bool> =
            conn.query_row("SELECT dismas_match(NULL, 'bach', '', NULL)", [], |row| {
                row.get(0)
            });
        assert!(result.is_err());
    }

    // Times searching a library of 50,000 tracks with plain words, as
    // before the search syntax, with `instr()`, and through `dismas_match()`:
    //   cargo test --release time_library_searches -- --ignored --nocapture
    #[test]
    #[ignore]
    fn time_library_searches() {
        let conn = test_db();
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..50_000 {
            let composer = [
                "johann sebastian bach",
                "georg friedrich handel",
                "antonio vivaldi",
            ][i % 3];
            let search = format!(
                "{composer} cantata no. {i} for soloists, choir and orchestra recorded live {}",
                "with a long comment about the recording and the performers ".repeat(3)
            );
            let fields = format!(
                "ti\tcantata no. {i}\nco\t{composer}\nyr\t{}\nge\tbaroque\nft\tflac\nln\t{}\n",
                1960 + i % 60,
                200 + i % 300
            );
            tx.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, search, fields)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![format!("/music/{i}.flac").as_bytes(), (i / 10).to_string(), search, fields],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let median = |run: &dyn Fn() -> usize| {
            let mut times: Vec<std::time::Duration> = (0..7)
                .map(|_| {
                    let start = std::time::Instant::now();
                    assert!(run() > 0);
                    start.elapsed()
                })
                .collect();
            times.sort();
            times[3]
        };
        let before = median(&|| {
            conn.prepare(
                "SELECT album_id, SUM(CASE WHEN instr(search, ?) > 0 THEN 1 ELSE 0 END) AS matching,
                        COUNT(*)
                 FROM tracks WHERE album_id IS NOT NULL GROUP BY album_id HAVING matching > 0",
            )
            .unwrap()
            .query_map(["bach"], |_| Ok(()))
            .unwrap()
            .count()
        });
        let search = |text: &str| {
            let filter = super::TrackFilter {
                query: super::TrackFilter::query(text),
                ..Default::default()
            };
            median(&|| {
                super::query_search_albums(&conn, &filter, &[])
                    .unwrap()
                    .len()
            })
        };
        let words = search("bach");
        let fields = search("composer:bach year:<1990");
        println!(
            "instr: {:.1} ms; dismas_match, words: {:.1} ms, fields: {:.1} ms",
            before.as_secs_f64() * 1000.0,
            words.as_secs_f64() * 1000.0,
            fields.as_secs_f64() * 1000.0
        );
        assert!(words.as_secs_f64() <= before.as_secs_f64() * 1.5);
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
        scan_roots(&mut conn, &roots, &sender).unwrap();
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
        let (result, _) = scan_roots(&mut conn, &roots, &sender).unwrap();
        assert_eq!(result.n_changed, 2);
        assert_eq!(paths(&conn), vec!["Album/One.wav"]);

        // And read again once it's back
        let (result, _) = scan_roots(&mut conn, &super::roots(&[folder(&[])]), &sender).unwrap();
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
        assert_eq!(relative_to(&folder(dir.clone()), &folder(music)), None);
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

        let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
        assert_eq!(result.n_changed, 3);
        assert_eq!(
            paths(&conn),
            vec!["A/One.wav", "A/sub/Two.wav", "B/Three.wav"]
        );

        // A folder inside another one is read once, either way round
        for roots in [roots_of(&[&a, &sub, &b]), roots_of(&[&sub, &a, &b])] {
            let (result, _) = scan_roots(&mut conn, &roots, &sender).unwrap();
            assert_eq!(result.n_changed, 0);
            assert_eq!(paths(&conn).len(), 3);
        }

        // The tracks of a folder that isn't there are kept, as are those of
        // an empty one, which is often a drive that isn't mounted
        let away = dir.join("B-away");
        std::fs::rename(&b, &away).unwrap();
        let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
        assert_eq!(result.n_changed, 0);
        assert_eq!(paths(&conn).len(), 3);
        std::fs::rename(&away, &b).unwrap();
        std::fs::rename(b.join("Three.wav"), dir.join("Three.wav")).unwrap();
        let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
        assert_eq!(result.n_changed, 0);
        assert_eq!(paths(&conn).len(), 3);

        // Those of a folder that was removed from the list go
        let (result, _) = scan_roots(&mut conn, &roots_of(&[&a]), &sender).unwrap();
        assert_eq!(result.n_changed, 1);
        assert_eq!(paths(&conn), vec!["A/One.wav", "A/sub/Two.wav"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_linked_to_another_is_read_once() {
        use crate::utils::write_test_wav;

        let dir = test_dir("link");
        let a = dir.join("A");
        std::fs::create_dir_all(&a).unwrap();
        write_test_wav(&a.join("One.wav"), 1);
        let b = dir.join("B");
        std::os::unix::fs::symlink(&a, &b).unwrap();

        let mut conn = test_db();
        let (sender, _receiver) = async_channel::unbounded();
        let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
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
        scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
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
        scan_roots(&mut conn, &roots_of(&[&dir]), &sender).unwrap();
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

        // An album's tracks, and the album of a track, leave them out too
        let all = super::TrackFilter::default();
        let sub = [PathBuf::from("/m/B/sub")];
        let three = Path::new("/m/B/sub/Three.flac");
        let album_tracks = |hidden: &[PathBuf]| {
            super::query_album_tracks(&conn, "b", &all, hidden)
                .unwrap()
                .into_iter()
                .map(|(_, path)| path)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            album_tracks(&[]),
            [PathBuf::from("/m/B/Two.flac"), three.to_path_buf()]
        );
        assert_eq!(album_tracks(&sub), [PathBuf::from("/m/B/Two.flac")]);
        assert_eq!(
            super::query_track_album(&conn, three, &[])
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(super::query_track_album(&conn, three, &sub).unwrap(), None);
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
        let list = watch_list(&folders);
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
        let list = watch_list(&folders);
        assert!(list.unavailable);
        std::fs::remove_file(gone).unwrap();

        // A folder that came through the document portal isn't watched
        let folders = [MusicFolder {
            host: Some(PathBuf::from("/media/music")),
            ..MusicFolder::new(music.clone())
        }];
        let list = watch_list(&folders);
        assert!(list.folders.is_empty());
        assert!(list.unwatched);

        // A drive mounted anywhere can bring back a folder that isn't there
        let folders = [MusicFolder::new(music.join("Unplugged"))];
        let list = watch_list(&folders);
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
    fn albums_are_on_several_discs_by_number_or_folder() {
        use super::Disc;

        let conn = test_db();
        for (path, album, disc) in [
            // Disc numbers, the folder aside
            ("/m/Box/1-01.flac", "box", Some(1)),
            ("/m/Box/2-01.flac", "box", Some(2)),
            // One disc, numbered or not
            ("/m/One/01.flac", "one", Some(1)),
            ("/m/One/02.flac", "one", None),
            // Disc folders without numbers
            ("/m/Opera/CD 1/01.flac", "opera", None),
            ("/m/Opera/CD 2/01.flac", "opera", None),
            // A disc that's hidden
            ("/m/Hidden/CD 1/01.flac", "hidden", None),
            ("/m/Gone/CD 2/01.flac", "hidden", None),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, disc)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
                rusqlite::params![path.as_bytes(), album, disc],
            )
            .unwrap();
        }
        let gone = [PathBuf::from("/m/Gone")];
        let all = super::TrackFilter::default();
        let has_discs = |album| {
            super::on_several_discs(&super::query_album_tracks(&conn, album, &all, &gone).unwrap())
        };
        assert!(has_discs("box"));
        assert!(!has_discs("one"));
        assert!(has_discs("opera"));
        assert!(!has_discs("hidden"));
        assert!(!has_discs("missing"));

        let folder = Path::new("/m/Opera/Disc 2 - Act III");
        assert_eq!(Disc::of(Some(3), Some(folder)), Disc::Number(3));
        assert_eq!(
            Disc::of(None, Some(folder)),
            Disc::Folder(folder.to_path_buf())
        );
        assert_eq!(Disc::of(None, Some(Path::new("/m/Opera"))), Disc::Number(1));
        assert_eq!(Disc::of(None, None), Disc::Number(1));
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
    fn the_composers_of_a_work_take_the_sort_names_of_their_tags() {
        use super::credited_work_composers;
        use crate::audio::TrackCredits;
        let names = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
        let sorts = |credits: &TrackCredits, artist| {
            credited_work_composers(credits, artist)
                .into_iter()
                .map(|composer| (composer.key, composer.sort))
                .collect::<Vec<_>>()
        };

        // The work's composers, matched to the track's sort names by name
        let requiem = TrackCredits {
            composers: names(&["Franz Xaver Süßmayr", "Wolfgang Amadeus Mozart"]),
            composer_sorts: names(&["Süßmayr, Franz Xaver", "Mozart, W. A."]),
            work_composers: names(&["Wolfgang Amadeus Mozart"]),
            ..Default::default()
        };
        assert_eq!(
            sorts(&requiem, None),
            [(
                "wolfgang amadeus mozart".to_string(),
                "Mozart, W. A.".to_string()
            )]
        );

        // Without as many sort names as names, they're worked out
        let suk = TrackCredits {
            composers: names(&["Antonín Dvořák", "Josef Suk"]),
            composer_sorts: names(&["Suk, Josef"]),
            ..Default::default()
        };
        assert_eq!(
            sorts(&suk, None),
            [
                ("antonin dvorak".to_string(), "Dvořák, Antonín".to_string()),
                ("josef suk".to_string(), "Suk, Josef".to_string())
            ]
        );

        // Without composers, the artist, if one is given
        let untagged = TrackCredits::default();
        assert_eq!(
            sorts(&untagged, Some("Arcangelo Corelli")),
            [(
                "arcangelo corelli".to_string(),
                "Corelli, Arcangelo".to_string()
            )]
        );
        assert!(sorts(&untagged, None).is_empty());
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
        let listed: Vec<(String, String)> = super::query_composers(
            &conn,
            &[],
            &super::TrackFilter::default(),
            super::Counted::Albums,
        )
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
    fn albums_in_the_folders_that_are_gone_are_found() {
        let conn = test_db();
        for (path, album) in [("/m/Gone/01.opus", Some("a")), ("/m/Loose/01.opus", None)] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2)",
                rusqlite::params![path.as_bytes(), album],
            )
            .unwrap();
        }
        assert!(super::query_tracks_have(&conn, "album_id", &[]).unwrap());
        // A track in no album, which the scan stores for a file it couldn't
        // read, makes no album
        let gone = [PathBuf::from("/m/Gone")];
        assert!(!super::query_tracks_have(&conn, "album_id", &gone).unwrap());
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
        assert!(super::query_tracks_have(&conn, "composers", &hidden).unwrap());
        let only_anonymous = [
            PathBuf::from("/m/A"),
            PathBuf::from("/m/B"),
            PathBuf::from("/m/Gone"),
        ];
        assert!(!super::query_tracks_have(&conn, "composers", &only_anonymous).unwrap());

        let all = super::TrackFilter::default();
        let composers =
            super::query_composers(&conn, &hidden, &all, super::Counted::Albums).unwrap();
        let listed: Vec<(&str, u32)> = composers
            .iter()
            .map(|composer| (composer.sort.as_str(), composer.count))
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
                query: super::TrackFilter::query(&words.join(" ")),
                ..Default::default()
            };
            super::query_composers(&conn, &hidden, &filter, super::Counted::Albums)
                .unwrap()
                .into_iter()
                .map(|composer| (composer.sort, composer.count))
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
            query: super::TrackFilter::query("1002"),
            ..filter
        };
        let albums = super::query_search_albums(&conn, &filter, &hidden).unwrap();
        assert_eq!(albums.get("a"), Some(&(1, 2)));
        assert_eq!(albums.len(), 1);
    }

    #[test]
    fn the_favorite_albums_are_found_with_the_other_filters() {
        let conn = test_db();
        let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
        for (path, album, search) in [
            ("/m/A/01.opus", "a", "bwv 1001"),
            ("/m/A/02.opus", "a", "bwv 1002"),
            ("/m/B/01.opus", "b", "bwv 1003"),
            ("/m/C/01.opus", "c", "bwv 1004"),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![path.as_bytes(), album, bach, search],
            )
            .unwrap();
        }
        let favorites = |ids: &[&str]| {
            Some(super::Favorites::Albums(
                ids.iter().map(ToString::to_string).collect(),
            ))
        };
        let albums = |filter: &super::TrackFilter| {
            let mut albums: Vec<(String, (u32, u32))> =
                super::query_search_albums(&conn, filter, &[])
                    .unwrap()
                    .into_iter()
                    .collect();
            albums.sort();
            albums
        };

        let filter = super::TrackFilter {
            favorites: favorites(&["a", "c"]),
            ..Default::default()
        };
        assert!(!filter.is_empty());
        assert_eq!(
            albums(&filter),
            [("a".to_string(), (2, 2)), ("c".to_string(), (1, 1))]
        );
        // With the words of a search, and the composer's list follows
        let filter = super::TrackFilter {
            query: super::TrackFilter::query("1002"),
            ..filter
        };
        assert_eq!(albums(&filter), [("a".to_string(), (1, 2))]);
        let composers =
            super::query_composers(&conn, &[], &filter, super::Counted::Albums).unwrap();
        assert_eq!(composers.len(), 1);
        assert_eq!(composers[0].count, 1);

        // Without favorites, nothing
        let filter = super::TrackFilter {
            favorites: favorites(&[]),
            ..Default::default()
        };
        assert!(albums(&filter).is_empty());
    }

    #[test]
    fn the_favorite_tracks_are_found_with_the_other_filters() {
        let conn = test_db();
        for (path, album, search) in [
            ("/m/A/01.opus", "a", "bwv 1001"),
            ("/m/A/02.opus", "a", "bwv 1002"),
            ("/m/B/01.opus", "b", "bwv 1003"),
            ("/m/C/01.opus", "c", "bwv 1004"),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
                rusqlite::params![path.as_bytes(), album, search],
            )
            .unwrap();
        }
        let favorites = |paths: &[&str]| {
            Some(super::Favorites::Tracks(
                paths.iter().map(PathBuf::from).collect(),
            ))
        };
        let albums = |filter: &super::TrackFilter| {
            let mut albums: Vec<(String, (u32, u32))> =
                super::query_search_albums(&conn, filter, &[])
                    .unwrap()
                    .into_iter()
                    .collect();
            albums.sort();
            albums
        };

        // One of the two tracks of an album, and the only one of another
        let filter = super::TrackFilter {
            favorites: favorites(&["/m/A/02.opus", "/m/C/01.opus"]),
            ..Default::default()
        };
        assert_eq!(
            albums(&filter),
            [("a".to_string(), (1, 2)), ("c".to_string(), (1, 1))]
        );
        // and only that track is taken from the album
        let tracks = super::query_album_tracks(&conn, "a", &filter, &[]).unwrap();
        assert_eq!(tracks, [(None, PathBuf::from("/m/A/02.opus"))]);

        // With the words of a search, only the favorites they match
        let words = |words: &[&str]| super::TrackFilter {
            query: super::TrackFilter::query(&words.join(" ")),
            ..filter.clone()
        };
        assert_eq!(albums(&words(&["1004"])), [("c".to_string(), (1, 1))]);
        assert!(albums(&words(&["1001"])).is_empty());

        // Without favorites, nothing
        let filter = super::TrackFilter {
            favorites: favorites(&[]),
            ..Default::default()
        };
        assert!(albums(&filter).is_empty());
    }

    #[test]
    fn a_random_track_is_one_the_filter_matches() {
        let conn = test_db();
        for (path, album, title, search) in [
            ("/m/A/01.opus", Some("a"), Some("Sonata"), "bwv 1001"),
            ("/m/A/02.opus", Some("a"), None, "bwv 1002"),
            ("/m/B/01.opus", Some("b"), Some("Partita"), "bwv 1004"),
            ("/m/Gone/01.opus", Some("g"), Some("Gone"), "bwv 1003"),
            // A file whose tags couldn't be read is in no album
            ("/m/X/01.opus", None, None, "bwv 1005"),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, title, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![path.as_bytes(), album, title, search],
            )
            .unwrap();
        }
        let hidden = [PathBuf::from("/m/Gone")];
        let tracks = |filter: &super::TrackFilter| {
            let mut tracks = super::query_matching_tracks(&conn, filter, &hidden)
                .unwrap()
                .into_iter()
                .map(|(path, title)| (path.to_string_lossy().into_owned(), title))
                .collect::<Vec<_>>();
            tracks.sort();
            tracks
        };
        let track =
            |path: &str, title: Option<&str>| (path.to_string(), title.map(ToString::to_string));

        assert_eq!(
            tracks(&super::TrackFilter::default()),
            [
                track("/m/A/01.opus", Some("Sonata")),
                track("/m/A/02.opus", None),
                track("/m/B/01.opus", Some("Partita")),
            ]
        );
        // The favorites, alone and with the words of a search
        let favorites = super::TrackFilter {
            favorites: Some(super::Favorites::Tracks(
                ["/m/A/02.opus", "/m/B/01.opus"]
                    .iter()
                    .map(PathBuf::from)
                    .collect(),
            )),
            ..Default::default()
        };
        assert_eq!(
            tracks(&favorites),
            [
                track("/m/A/02.opus", None),
                track("/m/B/01.opus", Some("Partita")),
            ]
        );
        let words = super::TrackFilter {
            query: super::TrackFilter::query("1004"),
            ..favorites
        };
        assert_eq!(tracks(&words), [track("/m/B/01.opus", Some("Partita"))]);
    }

    #[test]
    fn the_works_page_lists_only_the_people_of_works() {
        use super::Counted::{Albums, Works};

        let conn = test_db();
        let vivaldi = "\nantonio vivaldi\tAntonio Vivaldi\tVivaldi, Antonio\n";
        let violinist = "\ns\tenrico onofri\tEnrico Onofri\tOnofri, Enrico\tviolin\n";
        let conductor = "\nc\tgiovanni antonini\tGiovanni Antonini\tAntonini, Giovanni\t\n";
        // A sonata tagged with its work, and a concerto whose tracks only
        // name it in their titles
        for (path, album, work, performers) in [
            ("/m/A/01.opus", "a", Some("t:sonata"), violinist),
            ("/m/A/02.opus", "a", Some("t:sonata"), violinist),
            ("/m/B/01.opus", "b", None, conductor),
            ("/m/B/02.opus", "b", None, conductor),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers,
                                     performers, work_key, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, ?5, '')",
                rusqlite::params![path.as_bytes(), album, vivaldi, performers, work],
            )
            .unwrap();
        }

        let performers = |counted| -> Vec<(String, u32)> {
            let filter = super::TrackFilter {
                composer: Some("antonio vivaldi".to_string()),
                ..Default::default()
            };
            super::query_performers(&conn, &[], &filter, counted)
                .unwrap()
                .into_iter()
                .map(|performer| (performer.sort, performer.count))
                .collect()
        };
        assert_eq!(
            performers(Albums),
            [
                ("Antonini, Giovanni".to_string(), 1),
                ("Onofri, Enrico".to_string(), 1)
            ]
        );
        // The conductor of the concerto has no works to show
        assert_eq!(performers(Works), [("Onofri, Enrico".to_string(), 1)]);

        let composers = |performer: &str, counted| -> Vec<u32> {
            let filter = super::TrackFilter {
                performer: Some(performer.to_string()),
                ..Default::default()
            };
            super::query_composers(&conn, &[], &filter, counted)
                .unwrap()
                .into_iter()
                .map(|composer| composer.count)
                .collect()
        };
        assert_eq!(composers("giovanni antonini", Albums), [1]);
        assert!(composers("giovanni antonini", Works).is_empty());
        assert_eq!(composers("enrico onofri", Works), [1]);
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
        assert!(super::query_tracks_have(&conn, "performers", &hidden).unwrap());

        let all = super::TrackFilter::default();
        let performers =
            super::query_performers(&conn, &hidden, &all, super::Counted::Albums).unwrap();
        let listed: Vec<(PerformerKind, &str, &[String], u32)> = performers
            .iter()
            .map(|p| (p.kind, p.sort.as_str(), p.roles.as_slice(), p.count))
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
        let dvorak =
            super::query_performers(&conn, &hidden, &filter, super::Counted::Albums).unwrap();
        let listed: Vec<(PerformerKind, &str, u32)> = dvorak
            .iter()
            .map(|p| (p.kind, p.sort.as_str(), p.count))
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
    fn the_works_of_a_composer_are_in_the_order_of_their_catalogue() {
        use super::TrackFilter;

        let conn = test_db();
        let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
        let beethoven = "\nludwig van beethoven\tLudwig van Beethoven\tBeethoven, Ludwig van\n";
        let dvorak = "\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n";
        for (i, (name, composers)) in [
            ("Suite no. 1 in G major, BWV 1007", bach),
            ("A chorale without a number", bach),
            ("Partita no. 2 in D minor, BWV 1004", bach),
            ("Für Elise, WoO 59", beethoven),
            ("Symphony no. 3, op. 55 “Eroica”", beethoven),
            ("Piano Sonata no. 5, op. 10 no. 1", beethoven),
            ("Piano Sonata no. 1, op. 2 no. 1", beethoven),
            // Opus numbers are the most common, so the polka, with only a
            // B. number, comes after them
            ("Polka, B. 1", dvorak),
            ("Humoresques, op. 101", dvorak),
            ("Symphony no. 9, op. 95", dvorak),
            ("Slavonic Dances, op. 46, B. 83", dvorak),
        ]
        .into_iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, work_key,
                                     work, work_composers)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?3, ?4)",
                rusqlite::params![
                    format!("/m/{i}/01.flac").as_bytes(),
                    format!("album {i}"),
                    name,
                    composers
                ],
            )
            .unwrap();
        }

        let works = super::query_works(&conn, &TrackFilter::default(), &[]).unwrap();
        let names: Vec<_> = works.iter().map(|work| work.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Partita no. 2 in D minor, BWV 1004",
                "Suite no. 1 in G major, BWV 1007",
                "A chorale without a number",
                "Piano Sonata no. 1, op. 2 no. 1",
                "Piano Sonata no. 5, op. 10 no. 1",
                "Symphony no. 3, op. 55 “Eroica”",
                "Für Elise, WoO 59",
                "Slavonic Dances, op. 46, B. 83",
                "Symphony no. 9, op. 95",
                "Humoresques, op. 101",
                "Polka, B. 1",
            ]
        );
    }

    #[test]
    fn arrangements_are_listed_with_what_they_are() {
        use super::TrackFilter;

        let conn = test_db();
        let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
        let partita = "Violin Partita no. 2 in D minor, BWV 1004";
        for (path, album, work, arrangement_of, arrangement) in [
            ("/m/Violin/05.flac", "violin", "partita", None, None),
            // The catch-all for its arrangements, and one of its own
            (
                "/m/Lute/20.flac",
                "lute",
                "catch-all",
                Some(partita),
                Some(""),
            ),
            (
                "/m/Piano/01.flac",
                "piano",
                "busoni",
                Some(partita),
                Some("for piano, Busoni"),
            ),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, work_key,
                                     work, work_composers, movement_order,
                                     work_arrangement_of, work_arrangement)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, ?5, '5', ?6, ?7)",
                rusqlite::params![
                    path.as_bytes(),
                    album,
                    work,
                    partita,
                    bach,
                    arrangement_of,
                    arrangement
                ],
            )
            .unwrap();
        }

        let works = super::query_works(&conn, &TrackFilter::default(), &[]).unwrap();
        let labels: Vec<_> = works
            .iter()
            .map(|work| {
                (
                    work.key.as_str(),
                    work.arrangement.as_ref().map(super::Arrangement::shown),
                )
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("busoni", Some("for piano, Busoni".to_string())),
                ("catch-all", Some("arrangement".to_string())),
                ("partita", None),
            ]
        );
    }

    #[test]
    fn arrangements_are_listed_under_the_works_they_arrange() {
        use super::TrackFilter;

        let conn = test_db();
        let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
        let schubert = "\nfranz schubert\tFranz Schubert\tSchubert, Franz\n";
        let liszt = "\nfranz liszt\tFranz Liszt\tLiszt, Franz\n";
        let barber = "\nsamuel barber\tSamuel Barber\tBarber, Samuel\n";
        // (path, track, key, composers, order, the id of the work it
        // arranges, what it is, the ids of its parts, search)
        type Row<'a> = (
            &'a str,
            u32,
            &'a str,
            &'a str,
            Option<&'a str>,
            Option<&'a str>,
            &'a str,
            Option<&'a str>,
            &'a str,
        );
        let rows: &[Row] = &[
            // A partita, and the catch-all for its arrangements, whole in a
            // track after it on the same album and on an album of its own
            (
                "/m/violin/01",
                1,
                "partita",
                bach,
                Some("1"),
                None,
                "",
                Some("allemande"),
                "",
            ),
            (
                "/m/violin/02",
                2,
                "partita",
                bach,
                Some("5"),
                None,
                "",
                Some("chaconne"),
                "",
            ),
            (
                "/m/violin/03",
                3,
                "lute",
                bach,
                None,
                Some("partita"),
                "",
                None,
                "",
            ),
            (
                "/m/lute/01",
                1,
                "lute",
                bach,
                None,
                Some("partita"),
                "",
                None,
                "",
            ),
            // An arrangement of its own, and an arrangement of that one
            (
                "/m/piano/01",
                1,
                "busoni",
                bach,
                None,
                Some("partita"),
                "for piano, Busoni",
                None,
                "",
            ),
            (
                "/m/pianos/01",
                1,
                "pianos",
                bach,
                None,
                Some("busoni"),
                "for two pianos",
                None,
                "",
            ),
            // A song of a cycle arranged for a piano trio, which the tags
            // make a movement of the cycle, and a paraphrase of it credited
            // to the arranger, which stays on its own
            (
                "/m/lieder/04",
                4,
                "d957",
                schubert,
                Some("4"),
                None,
                "",
                Some("standchen"),
                "",
            ),
            (
                "/m/trio/13",
                13,
                "standchen",
                schubert,
                Some("4"),
                Some("standchen"),
                "",
                None,
                "trio",
            ),
            (
                "/m/liszt/07",
                7,
                "paraphrase",
                liszt,
                Some("4"),
                Some("standchen"),
                "",
                None,
                "",
            ),
            // A whole work that arranges a movement stays on its own
            (
                "/m/quartet/02",
                2,
                "op11",
                barber,
                Some("2"),
                None,
                "",
                Some("molto"),
                "",
            ),
            (
                "/m/strings/01",
                1,
                "adagio",
                barber,
                None,
                Some("molto"),
                "",
                None,
                "",
            ),
            // An arrangement whose original isn't in the library
            (
                "/m/other/01",
                1,
                "orphan",
                bach,
                None,
                Some("missing"),
                "",
                None,
                "",
            ),
            // A recording whose last movement MusicBrainz links to an
            // arrangement of the first
            (
                "/m/sonn/01",
                1,
                "bwv80",
                bach,
                Some("1"),
                None,
                "",
                Some("bwv80-1"),
                "",
            ),
            (
                "/m/sonn/02",
                2,
                "bwv80",
                bach,
                Some("2"),
                None,
                "",
                Some("bwv80-2"),
                "",
            ),
            (
                "/m/sonn/08",
                8,
                "chorale",
                bach,
                Some("8"),
                Some("bwv80-1"),
                "",
                None,
                "",
            ),
            // A song in two works, which an arrangement of it goes under
            // the first of by key, every time
            (
                "/m/cycle/01",
                1,
                "cycle",
                schubert,
                Some("1"),
                None,
                "",
                Some("song"),
                "",
            ),
            (
                "/m/set/01",
                1,
                "collection",
                schubert,
                Some("1"),
                None,
                "",
                Some("song"),
                "",
            ),
            (
                "/m/song/01",
                1,
                "song-arr",
                schubert,
                Some("1"),
                Some("song"),
                "",
                None,
                "",
            ),
            // Two arrangements of each other
            (
                "/m/loop/01",
                1,
                "loop-a",
                bach,
                None,
                Some("loop-b"),
                "",
                None,
                "",
            ),
            (
                "/m/loop/02",
                2,
                "loop-b",
                bach,
                None,
                Some("loop-a"),
                "",
                None,
                "",
            ),
        ];
        for (path, track, key, composers, order, of_id, label, parts, search) in rows {
            let album = path.split('/').nth(2).unwrap();
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album, track,
                                     search, work_key, work, work_composers, movement_order,
                                     work_arrangement_of, work_arrangement,
                                     work_arrangement_of_id, work_part_ids)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    path.as_bytes(),
                    album,
                    track,
                    search,
                    key,
                    composers,
                    order,
                    of_id.map(|_| "the work it arranges"),
                    of_id.map(|_| *label),
                    of_id,
                    parts
                ],
            )
            .unwrap();
        }
        let works = |filter: &TrackFilter| {
            let mut works = super::query_works(&conn, filter, &[])
                .unwrap()
                .into_iter()
                .map(|work| (work.key, work.recordings, work.arrangements))
                .collect::<Vec<_>>();
            works.sort();
            works
        };
        let strings = |keys: &[&str]| keys.iter().map(ToString::to_string).collect::<Vec<_>>();
        let work = |key: &str, recordings, arrangements: &[&str]| {
            (key.to_string(), recordings, strings(arrangements))
        };
        assert_eq!(
            works(&TrackFilter::default()),
            [
                work("adagio", 1, &[]),
                work("bwv80", 1, &["chorale"]),
                work("collection", 2, &["song-arr"]),
                work("cycle", 1, &[]),
                work("d957", 2, &["standchen"]),
                work("loop-a", 1, &[]),
                work("loop-b", 1, &[]),
                work("op11", 1, &[]),
                work("orphan", 1, &[]),
                work("paraphrase", 1, &[]),
                work("partita", 5, &["busoni", "lute", "pianos"]),
            ]
        );
        // A search that only finds the arrangement finds the work it's
        // listed under
        let trio = TrackFilter {
            query: super::TrackFilter::query("trio"),
            ..Default::default()
        };
        assert_eq!(works(&trio), [work("d957", 1, &["standchen"])]);

        // The recordings of a work and of its arrangements, each found by the
        // work of its own tracks, and saying what it is if it's all of an
        // arrangement; the movements of the arrangement are one recording
        // with those of the work, but the whole works are recordings of
        // their own
        let recordings = |keys: &[&str]| {
            super::query_work_recordings(&conn, &strings(keys), &TrackFilter::default(), &[])
                .unwrap()
                .into_iter()
                .map(|recording| {
                    let paths = recording
                        .paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    let arrangement = recording.arrangement.map(|a| a.shown());
                    (paths, recording.work_key, arrangement)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            recordings(&["bwv80", "chorale"]),
            [(
                strings(&["/m/sonn/01", "/m/sonn/02", "/m/sonn/08"]),
                "bwv80".to_string(),
                None
            )]
        );
        let mut partita = recordings(&["partita", "busoni", "lute", "pianos"]);
        partita.sort();
        assert_eq!(
            partita,
            [
                (
                    strings(&["/m/lute/01"]),
                    "lute".to_string(),
                    Some("arrangement".to_string())
                ),
                (
                    strings(&["/m/piano/01"]),
                    "busoni".to_string(),
                    Some("for piano, Busoni".to_string())
                ),
                (
                    strings(&["/m/pianos/01"]),
                    "pianos".to_string(),
                    Some("for two pianos".to_string())
                ),
                (
                    strings(&["/m/violin/01", "/m/violin/02"]),
                    "partita".to_string(),
                    None
                ),
                (
                    strings(&["/m/violin/03"]),
                    "lute".to_string(),
                    Some("arrangement".to_string())
                ),
            ]
        );
        // The covers of the work take the tracks of its arrangements
        let partita = TrackFilter {
            works: strings(&["partita", "busoni", "lute", "pianos"]),
            ..Default::default()
        };
        let mut albums = super::query_search_albums(&conn, &partita, &[])
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>();
        albums.sort();
        assert_eq!(
            albums,
            [
                ("lute".to_string(), (1, 1)),
                ("piano".to_string(), (1, 1)),
                ("pianos".to_string(), (1, 1)),
                ("violin".to_string(), (3, 3)),
            ]
        );

        // Play a Random Work picks from the works as they're listed, each
        // with the recordings that `work_recordings()` gives for its keys
        let sorted_paths = |recordings: Vec<(Vec<String>, String, Option<String>)>| {
            let mut recordings = recordings;
            recordings.sort();
            recordings
        };
        let strings_of = |paths: &[std::path::PathBuf]| {
            paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        for filter in [TrackFilter::default(), trio.clone()] {
            let candidates = super::query_work_recording_candidates(&conn, &filter, &[]).unwrap();
            let listed = super::query_works(&conn, &filter, &[]).unwrap();
            assert_eq!(
                candidates
                    .iter()
                    .map(|candidate| &candidate.info)
                    .collect::<Vec<_>>(),
                listed.iter().collect::<Vec<_>>()
            );
            for candidate in &candidates {
                assert_eq!(
                    candidate.recordings.len(),
                    candidate.info.recordings as usize
                );
                // The same tracks, of the same works, which are as much
                // arrangements
                let expected =
                    super::query_work_recordings(&conn, &candidate.info.keys(), &filter, &[])
                        .unwrap()
                        .into_iter()
                        .map(|recording| {
                            let arrangement = recording.arrangement.map(|a| a.shown());
                            (
                                strings_of(&recording.paths),
                                recording.work_key,
                                arrangement,
                            )
                        })
                        .collect();
                let found = candidate
                    .recordings
                    .iter()
                    .map(|recording| {
                        let arrangement = recording
                            .arrangement
                            .as_ref()
                            .map(super::Arrangement::shown);
                        let work_key = recording.work_key.clone();
                        (strings_of(&recording.paths), work_key, arrangement)
                    })
                    .collect();
                assert_eq!(
                    sorted_paths(found),
                    sorted_paths(expected),
                    "{}",
                    candidate.info.key
                );
            }
        }
        let candidates =
            super::query_work_recording_candidates(&conn, &TrackFilter::default(), &[]).unwrap();
        let partita = candidates
            .iter()
            .find(|candidate| candidate.info.key == "partita")
            .unwrap();
        assert_eq!(partita.recordings.len(), 5);
        // The arrangement on the lute goes with the work, as it's listed
        // under it
        assert!(partita.recordings.iter().any(|recording| {
            recording.work_key == "lute" && strings_of(&recording.paths) == ["/m/violin/03"]
        }));
        // The work of a search that only finds an arrangement plays the
        // arrangement, and the tracks of all its recordings tell when it was
        // heard
        let candidates = super::query_work_recording_candidates(&conn, &trio, &[]).unwrap();
        let [d957] = candidates.as_slice() else {
            panic!("{candidates:?}");
        };
        assert_eq!(d957.info.key, "d957");
        let [recording] = d957.recordings.as_slice() else {
            panic!("{d957:?}");
        };
        assert_eq!(recording.work_key, "standchen");
        assert!(d957.paths.len() > recording.paths.len());
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
            query: super::TrackFilter::query("graduale"),
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
            super::query_work_recordings(&conn, &[key.to_string()], filter, &hidden)
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
        // The tracks of a work on each album, as the covers of the albums of
        // its recordings take them, alone and with the words of a search
        let gold = super::TrackFilter {
            works: vec!["gold".to_string()],
            ..Default::default()
        };
        assert!(!gold.is_empty());
        let albums = |filter: &super::TrackFilter| {
            let mut albums = super::query_search_albums(&conn, filter, &hidden)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>();
            albums.sort();
            albums
        };
        assert_eq!(
            albums(&gold),
            [("b".to_string(), (4, 4)), ("e".to_string(), (1, 5))]
        );
        let variations = super::TrackFilter {
            query: super::TrackFilter::query("variatio"),
            ..gold.clone()
        };
        assert_eq!(albums(&variations), [("b".to_string(), (2, 4))]);
        let tracks = super::query_album_tracks(&conn, "e", &gold, &hidden)
            .unwrap()
            .into_iter()
            .map(|(_, path)| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(tracks, strings(&["/m/E/03"]));

        // The work of a track
        let work = |path: &str| super::query_track_work(&conn, Path::new(path), &hidden).unwrap();
        assert_eq!(work("/m/B/04").as_deref(), Some("gold"));
        assert_eq!(work("/m/A/01").as_deref(), Some("req"));
        assert_eq!(work("/m/A/03"), None);
        assert_eq!(work("/m/Gone/01").as_deref(), Some("req"));
        assert_eq!(
            recordings("kdf", &all),
            [(
                None,
                strings(&["/m/E/01", "/m/E/02", "/m/E/04", "/m/E/05"]),
                String::new()
            )]
        );

        // The works as Play a Random Work picks from them, each with the
        // recordings of it that the filter matches
        let candidates = |filter: &super::TrackFilter| {
            super::query_work_recording_candidates(&conn, filter, &hidden)
                .unwrap()
                .into_iter()
                .map(|work| {
                    let paths = |paths: &[PathBuf]| {
                        paths
                            .iter()
                            .map(|path| path.to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                    };
                    let recordings = work
                        .recordings
                        .iter()
                        .map(|recording| paths(&recording.paths))
                        .collect::<Vec<_>>();
                    (work.info.name, recordings, paths(&work.paths))
                })
                .collect::<Vec<_>>()
        };
        let kdf = strings(&["/m/E/01", "/m/E/02", "/m/E/04", "/m/E/05"]);
        let goldberg = strings(&["/m/B/01", "/m/B/02", "/m/B/03", "/m/B/04", "/m/E/03"]);
        let requiem = strings(&["/m/A/01", "/m/A/02", "/m/C/01", "/m/C/02"]);
        assert_eq!(
            candidates(&all),
            [
                ("Die Kunst der Fuge".to_string(), vec![kdf.clone()], kdf),
                (
                    "Goldberg Variations".to_string(),
                    vec![
                        strings(&["/m/B/01", "/m/B/02", "/m/B/03"]),
                        strings(&["/m/B/04"]),
                        strings(&["/m/E/03"]),
                    ],
                    goldberg
                ),
                (
                    "Requiem v B-moll".to_string(),
                    vec![
                        strings(&["/m/A/01", "/m/A/02"]),
                        strings(&["/m/C/01", "/m/C/02"])
                    ],
                    requiem.clone()
                ),
                (
                    "Stabat Mater".to_string(),
                    vec![strings(&["/m/A/04"])],
                    strings(&["/m/A/04"])
                ),
                (
                    "Symphony no. 2".to_string(),
                    vec![strings(&["/m/D/02"])],
                    strings(&["/m/D/02"])
                ),
                (
                    "Symphony no. 10".to_string(),
                    vec![strings(&["/m/D/01"])],
                    strings(&["/m/D/01"])
                ),
            ]
        );
        // Only the recordings the search matches, but the tracks of all of
        // them, which tell when the work was heard last
        assert_eq!(
            candidates(&graduale),
            [(
                "Requiem v B-moll".to_string(),
                vec![strings(&["/m/A/01", "/m/A/02"])],
                requiem
            )]
        );
        // The recordings that are a single movement of their work: not two
        // movements, nor the parts of one with another, nor movements whose
        // places aren't known, nor a whole work
        let single_movements = super::query_work_recording_candidates(&conn, &all, &hidden)
            .unwrap()
            .into_iter()
            .flat_map(|work| work.recordings)
            .filter(|recording| recording.single_movement)
            .map(|recording| {
                recording
                    .paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            single_movements,
            [strings(&["/m/B/04"]), strings(&["/m/E/03"])]
        );
    }

    #[test]
    fn a_recording_is_as_long_as_its_tracks() {
        let conn = test_db();
        let length = |fields: Option<&str>| -> Option<i64> {
            conn.query_row("SELECT dismas_length(?1)", [fields], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(length(Some("ln\t300\nsr\t44100\n")), Some(300));
        assert_eq!(length(Some("ti\tadagio\nln\t42\nsr\t44100\n")), Some(42));
        assert_eq!(length(Some("ti\tadagio\nln\t7")), Some(7));
        assert_eq!(length(Some("ti\tadagio\n")), None);
        assert_eq!(length(None), None);

        // A movement whose length isn't known counts as long as the others
        // of its recording; a recording with no length known has none
        for (path, album, order, fields) in [
            ("/m/A/01", "a", "1", Some("ti\tlargo\nln\t100\n")),
            ("/m/A/02", "a", "2", Some("ln\t200\n")),
            ("/m/A/03", "a", "3", None),
            ("/m/B/01", "b", "1", None),
        ] {
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                     work_key, work, movement_order, fields)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, 'w', 'Work', ?3, ?4)",
                rusqlite::params![path.as_bytes(), album, order, fields],
            )
            .unwrap();
        }
        let candidates =
            super::query_work_recording_candidates(&conn, &super::TrackFilter::default(), &[])
                .unwrap();
        let [work] = candidates.as_slice() else {
            panic!("{candidates:?}");
        };
        let lengths = work
            .recordings
            .iter()
            .map(|recording| recording.length)
            .collect::<Vec<_>>();
        assert_eq!(lengths, [Some(450), None]);
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
        let tags = super::TrackTags::from_tag(
            &tag,
            path,
            Some(path),
            &ExtraComments::default(),
            (None, None),
        );
        assert_eq!(tags.year.as_deref(), Some("1961"));
        assert_eq!(tags.label.as_deref(), Some("Hyperion"));
        assert_eq!(tags.catalog_number.as_deref(), Some("CDA67100"));
        let extra = ExtraComments::with_recording_date("1960-05/1960-06");
        let tags = super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None));
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
