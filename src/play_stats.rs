// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! What has been played, and the favorites, for the smart playlists
//!
//! Each listen to a track is logged with the seconds heard, in a database
//! of its own: the library's is a cache that is made again when its schema
//! changes, and neither the history nor the favorites can be read again
//! from anywhere. The log keeps the last year; the totals of each track
//! are kept for good.

use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gtk::glib;
use log::{debug, warn};
use rusqlite::{Connection, TransactionBehavior, params};

const SCHEMA_VERSION: i32 = 4;

// Tracks are told apart by their URI, as for the resume points; a moved
// or renamed file starts with no stats
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS listens (
        uri TEXT NOT NULL,
        -- Seconds since the epoch
        ended INTEGER NOT NULL,
        -- Seconds heard
        heard INTEGER NOT NULL,
        counted INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS listens_ended ON listens (ended);
    CREATE TABLE IF NOT EXISTS totals (
        uri TEXT PRIMARY KEY,
        plays INTEGER NOT NULL,
        heard INTEGER NOT NULL,
        last_played INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS favorites (
        uri TEXT PRIMARY KEY,
        -- Seconds since the epoch, for the order of the list
        added INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS favorite_works (
        -- What the library tells the work apart by, see `library::WorkInfo`
        key TEXT PRIMARY KEY,
        added INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS favorite_recordings (
        work_key TEXT NOT NULL,
        album_id TEXT NOT NULL,
        -- The URI of its first track, which tells two recordings of the
        -- work on one album apart
        uri TEXT NOT NULL,
        added INTEGER NOT NULL,
        PRIMARY KEY (work_key, album_id, uri)
    );
    CREATE TABLE IF NOT EXISTS favorite_albums (
        -- What the library tells the album apart by, see
        -- `library::AlbumInfo`
        album_id TEXT PRIMARY KEY,
        -- The URI of its first track when it was marked, which finds the
        -- album again once a change to its tags gives it another id; NULL
        -- until it's been looked up in the library
        uri TEXT,
        added INTEGER NOT NULL
    );";

const DAY: i64 = 24 * 60 * 60;
// How long the log keeps the listens
const KEPT: i64 = 365 * DAY;
// How far back Most Played looks
const MOST_PLAYED_SPAN: i64 = 182 * DAY;
// How long the tracks of Not Heard Lately haven't been heard, about 2
// months, and the plays they need, all time, to have been played often
const NOT_HEARD_SPAN: i64 = 61 * DAY;
const NOT_HEARD_PLAYS: u32 = 3;
// The number of tracks in a smart playlist
const SMART_PLAYLIST_SIZE: i64 = 100;

// A listen is logged from 30 s, or half of a shorter track
const LOGGED: u64 = 30;
// and counts as a play from half the track, or 4 minutes if that's less,
// as on Last.fm
const PLAYED: u64 = 4 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listen {
    pub uri: String,
    // Seconds since the epoch
    pub ended: i64,
    // Seconds heard
    pub heard: u64,
    // Whether it counts as a play
    pub counted: bool,
}

// What was heard of a track, from its totals
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackStats {
    pub plays: u32,
    // Seconds since the epoch: the end of the last listen logged, whether
    // it counted as a play or not
    pub last_heard: i64,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// The seconds a listen needs to be logged, and to count as a play; a
// track of unknown length needs 30 s and 4 minutes
fn thresholds(duration: u64) -> (u64, u64) {
    if duration == 0 {
        return (LOGGED, PLAYED);
    }
    let half = duration.div_ceil(2);
    (half.min(LOGGED), half.min(PLAYED))
}

// Measures how long the track playing is heard, from the positions the
// player reports. They're whole seconds, sent four times a second, so a
// step of a second or two is playback; seeks, resume points and going
// back to the start jump further, or backwards, and add nothing
#[derive(Debug, Default)]
pub struct ListenTracker {
    // The URI and the duration of the track
    track: Option<(String, u64)>,
    heard: u64,
    last: Option<u64>,
}

impl ListenTracker {
    pub fn start(&mut self, uri: String, duration: u64) {
        self.track = Some((uri, duration));
        self.heard = 0;
        self.last = None;
    }

    pub fn advance(&mut self, position: u64, playing: bool) {
        if self.track.is_none() {
            return;
        }
        if playing
            && let Some(step) = self.last.and_then(|last| position.checked_sub(last))
            && (1..=2).contains(&step)
        {
            self.heard += step;
        }
        self.last = Some(position);
    }

    // The listen so far, if it was heard long enough to log. The track
    // stays, so playing it again, e.g. once the queue finished, is another
    // listen
    pub fn finish(&mut self, ended: i64) -> Option<Listen> {
        let heard = std::mem::take(&mut self.heard);
        let (uri, duration) = self.track.as_ref()?;
        let (logged, played) = thresholds(*duration);
        (heard >= logged).then(|| Listen {
            uri: uri.clone(),
            ended,
            heard,
            counted: heard >= played,
        })
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

fn data_dir() -> PathBuf {
    glib::user_data_dir().join("dismas")
}

// Each thread keeps its connection, as the library's do: the listens are
// written in the threads of GLib's pool, or on the main thread when quitting
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
    // The history is the user's own, as the XDG spec asks of the folders
    // an app makes
    glib::mkdir_with_parents(&dir, 0o700);

    let mut conn = Connection::open(dir.join("plays.db"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrate(&mut conn)?;
    Ok(conn)
}

// Unlike the library, the tables are never made again: a later version
// changes them in place. Version 2 only adds the favorites table,
// version 3 the favorite works and recordings, and version 4 the favorite
// albums, as each statement of the schema leaves a table that's already
// there as it is.
// A database from a newer Dismas is left as it is
fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    // Another thread can be opening the database too, so the version is
    // checked again once this one has the database to itself
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < SCHEMA_VERSION {
        debug!("Play stats schema version {version}, updating it to {SCHEMA_VERSION}");
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()
}

// Bumped each time the history is cleared, before it is: a listen that
// ended before, still being written in the background, is dropped
static HISTORY_GENERATION: AtomicU64 = AtomicU64::new(0);

// What a listen that ends now is recorded with
pub fn history_generation() -> u64 {
    HISTORY_GENERATION.load(Ordering::SeqCst)
}

// Logs the listen, adds it to the totals of its track, and drops the
// listens older than a year; unless the history was cleared since the
// listen ended, at `generation`. That's checked once the database is this
// thread's to write, as clearing it bumps the generation first
pub fn record(listen: &Listen, generation: u64) {
    let result = with_connection(|conn| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if history_generation() != generation {
            debug!(
                "Dropping the listen to {}, from before the history was cleared",
                listen.uri
            );
            return Ok(());
        }
        write_listen(&tx, listen)?;
        tx.commit()
    });
    if let Err(e) = result {
        warn!("Unable to record the listen to {}: {e}", listen.uri);
    }
}

#[cfg(test)]
fn insert_listen(conn: &mut Connection, listen: &Listen) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    write_listen(&tx, listen)?;
    tx.commit()
}

fn write_listen(tx: &rusqlite::Transaction, listen: &Listen) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO listens (uri, ended, heard, counted) VALUES (?1, ?2, ?3, ?4)",
        params![
            listen.uri,
            listen.ended,
            listen.heard as i64,
            listen.counted
        ],
    )?;
    // The listens are written in different threads, and the last of two
    // close together can be written first
    tx.execute(
        "INSERT INTO totals (uri, plays, heard, last_played) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (uri) DO UPDATE SET
             plays = plays + excluded.plays,
             heard = heard + excluded.heard,
             last_played = MAX(last_played, excluded.last_played)",
        params![
            listen.uri,
            i64::from(listen.counted),
            listen.heard as i64,
            listen.ended
        ],
    )?;
    tx.execute(
        "DELETE FROM listens WHERE ended < ?1",
        [listen.ended - KEPT],
    )?;
    Ok(())
}

// Forgets the listens and the totals of the tracks, but not the favorites,
// which are marked rather than heard
pub fn clear_history() -> rusqlite::Result<()> {
    HISTORY_GENERATION.fetch_add(1, Ordering::SeqCst);
    with_connection(delete_history)
}

fn delete_history(conn: &mut Connection) -> rusqlite::Result<()> {
    // What was deleted is overwritten in the file, rather than left in
    // pages that are only marked as free, and the log of the changes is
    // emptied once they're in it
    conn.pragma_update(None, "secure_delete", true)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM listens", [])?;
    tx.execute("DELETE FROM totals", [])?;
    tx.commit()?;
    // A checkpoint that another connection holds up doesn't fail: its first
    // column says it was blocked. The next one empties the log then
    match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(0) => {}
        Ok(_) => debug!("Unable to empty the log of the play stats: it's in use"),
        Err(e) => debug!("Unable to empty the log of the play stats: {e}"),
    }
    Ok(())
}

// The URIs of the last tracks heard, in the order they were last heard,
// so that the movements of a work play in their order
pub fn recently_played() -> Vec<String> {
    match with_connection(|conn| query_recently_played(conn)) {
        Ok(uris) => uris,
        Err(e) => {
            warn!("Unable to load the recently played tracks: {e}");
            Vec::new()
        }
    }
}

fn query_recently_played(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT uri FROM (
             SELECT uri, MAX(ended) AS last, MAX(rowid) AS logged FROM listens
             GROUP BY uri ORDER BY last DESC, logged DESC LIMIT ?1
         ) ORDER BY last, logged",
    )?;
    stmt.query_map([SMART_PLAYLIST_SIZE], |row| row.get(0))?
        .collect()
}

// The URIs of the tracks played the most in the last 6 months. Tracks
// with the same number of plays are in the order they were last heard,
// so that a work heard through keeps its movements in order
pub fn most_played() -> Vec<String> {
    match with_connection(|conn| query_most_played(conn, now())) {
        Ok(uris) => uris,
        Err(e) => {
            warn!("Unable to load the most played tracks: {e}");
            Vec::new()
        }
    }
}

fn query_most_played(conn: &Connection, now: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT uri FROM (
             SELECT uri, COUNT(*) AS plays, MAX(ended) AS last, MAX(rowid) AS logged
             FROM listens WHERE counted AND ended >= ?1
             GROUP BY uri ORDER BY plays DESC, last DESC, logged DESC LIMIT ?2
         ) ORDER BY plays DESC, last, logged",
    )?;
    stmt.query_map([now - MOST_PLAYED_SPAN, SMART_PLAYLIST_SIZE], |row| {
        row.get(0)
    })?
    .collect()
}

// The URIs of the tracks played the most, all time, that haven't been
// heard in the last 2 months, even for less than a play. Tracks with the
// same number of plays are in the order they were last heard, so that a
// work heard through keeps its movements in order, and the ones kept are
// those that went unheard the longest
pub fn not_heard_lately() -> Vec<String> {
    match with_connection(|conn| query_not_heard_lately(conn, now())) {
        Ok(uris) => uris,
        Err(e) => {
            warn!("Unable to load the tracks not heard lately: {e}");
            Vec::new()
        }
    }
}

fn query_not_heard_lately(conn: &Connection, now: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT uri FROM totals WHERE plays >= ?1 AND last_played < ?2
         ORDER BY plays DESC, last_played LIMIT ?3",
    )?;
    stmt.query_map(
        params![NOT_HEARD_PLAYS, now - NOT_HEARD_SPAN, SMART_PLAYLIST_SIZE],
        |row| row.get(0),
    )?
    .collect()
}

// The totals of the tracks heard, by URI, for the shuffle; the tracks
// never heard are left out
pub fn track_stats(uris: &[String]) -> HashMap<String, TrackStats> {
    match with_connection(|conn| query_track_stats(conn, uris)) {
        Ok(stats) => stats,
        Err(e) => {
            warn!("Unable to load the play stats of the tracks: {e}");
            HashMap::new()
        }
    }
}

fn query_track_stats(
    conn: &mut Connection,
    uris: &[String],
) -> rusqlite::Result<HashMap<String, TrackStats>> {
    // A single read, rather than one for each track
    let tx = conn.transaction()?;
    let mut stats = HashMap::new();
    {
        let mut stmt = tx.prepare_cached("SELECT plays, last_played FROM totals WHERE uri = ?1")?;
        for uri in uris {
            let mut rows = stmt.query([uri])?;
            if let Some(row) = rows.next()? {
                let track = TrackStats {
                    plays: row.get(0)?,
                    last_heard: row.get(1)?,
                };
                stats.insert(uri.clone(), track);
            }
        }
    }
    tx.commit()?;
    Ok(stats)
}

// The favorites are read and written on the main thread, as each is a
// single row. If they were written in GLib's pool, like the listens, two
// clicks on the heart close together could be written in the wrong order
pub fn is_favorite(uri: &str) -> bool {
    match with_connection(|conn| query_is_favorite(conn, uri)) {
        Ok(favorite) => favorite,
        Err(e) => {
            warn!("Unable to find whether {uri} is a favorite: {e}");
            false
        }
    }
}

fn query_is_favorite(conn: &Connection, uri: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached("SELECT 1 FROM favorites WHERE uri = ?1")?
        .exists([uri])
}

pub fn set_favorite(uri: &str, favorite: bool) {
    let result = with_connection(|conn| {
        if favorite {
            insert_favorite(conn, uri, now())
        } else {
            delete_favorite(conn, uri)
        }
    });
    if let Err(e) = result {
        warn!("Unable to change whether {uri} is a favorite: {e}");
    }
}

// Marking a favorite again keeps its place in the list
fn insert_favorite(conn: &Connection, uri: &str, added: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO favorites (uri, added) VALUES (?1, ?2)",
        params![uri, added],
    )?;
    Ok(())
}

fn delete_favorite(conn: &Connection, uri: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM favorites WHERE uri = ?1", [uri])?;
    Ok(())
}

// The URIs of the favorites, in the order they were marked, so that the
// movements of a work marked one after another play in their order
pub fn favorites() -> Vec<String> {
    match with_connection(|conn| query_favorites(conn)) {
        Ok(uris) => uris,
        Err(e) => {
            warn!("Unable to load the favorite tracks: {e}");
            Vec::new()
        }
    }
}

fn query_favorites(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT uri FROM favorites ORDER BY added, rowid")?;
    stmt.query_map([], |row| row.get(0))?.collect()
}

// A recording of a work marked as a favorite: the work, the album, and
// the URI of the recording's first track, see `library::find_recording`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavoriteRecording {
    pub work_key: String,
    pub album_id: String,
    pub uri: String,
}

// What Favorite Works plays, in the order they were marked: each favorite
// recording, and each favorite work none of whose recordings is one, which
// plays one of its recordings
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FavoriteItem {
    Work(String),
    Recording(FavoriteRecording),
}

// The favorite works and recordings are read and written on the main
// thread too, like the favorite tracks
pub fn is_favorite_work(key: &str) -> bool {
    with_connection(|conn| {
        conn.prepare_cached("SELECT 1 FROM favorite_works WHERE key = ?1")?
            .exists([key])
    })
    .unwrap_or_else(|e| {
        warn!("Unable to find whether work {key} is a favorite: {e}");
        false
    })
}

pub fn set_favorite_work(key: &str, favorite: bool) {
    let result = with_connection(|conn| set_work(conn, key, favorite, now()));
    if let Err(e) = result {
        warn!("Unable to change whether work {key} is a favorite: {e}");
    }
}

// Marking one again keeps its place in the list, as for the tracks
fn set_work(conn: &Connection, key: &str, favorite: bool, added: i64) -> rusqlite::Result<usize> {
    if favorite {
        conn.execute(
            "INSERT OR IGNORE INTO favorite_works (key, added) VALUES (?1, ?2)",
            params![key, added],
        )
    } else {
        conn.execute("DELETE FROM favorite_works WHERE key = ?1", [key])
    }
}

pub fn is_favorite_recording(recording: &FavoriteRecording) -> bool {
    with_connection(|conn| {
        conn.prepare_cached(
            "SELECT 1 FROM favorite_recordings
             WHERE work_key = ?1 AND album_id = ?2 AND uri = ?3",
        )?
        .exists(params![
            recording.work_key,
            recording.album_id,
            recording.uri
        ])
    })
    .unwrap_or_else(|e| {
        warn!("Unable to find whether a recording is a favorite: {e}");
        false
    })
}

pub fn set_favorite_recording(recording: &FavoriteRecording, favorite: bool) {
    let result = with_connection(|conn| set_recording(conn, recording, favorite, now()));
    if let Err(e) = result {
        warn!("Unable to change whether a recording is a favorite: {e}");
    }
}

fn set_recording(
    conn: &Connection,
    recording: &FavoriteRecording,
    favorite: bool,
    added: i64,
) -> rusqlite::Result<usize> {
    let FavoriteRecording {
        work_key,
        album_id,
        uri,
    } = recording;
    if favorite {
        conn.execute(
            "INSERT OR IGNORE INTO favorite_recordings (work_key, album_id, uri, added)
             VALUES (?1, ?2, ?3, ?4)",
            params![work_key, album_id, uri, added],
        )
    } else {
        conn.execute(
            "DELETE FROM favorite_recordings WHERE work_key = ?1 AND album_id = ?2 AND uri = ?3",
            params![work_key, album_id, uri],
        )
    }
}

// What the search syntax looks into in plays.db, by URI, read only as far
// as a search needs it, see `search_query::Needs`
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    // The totals of the tracks heard, for `plays:` and `played:`
    pub heard: HashMap<String, TrackStats>,
    // When each favorite track was marked, for `is:favorite` and
    // `favorited:`
    pub favorites: HashMap<String, i64>,
    // The ids of the favorite albums, for `is:favorite-album`
    pub favorite_albums: BTreeSet<String>,
    // The keys of the favorite works, and of the works with a favorite
    // recording, for `is:favorite-work`
    pub favorite_works: std::collections::HashSet<String>,
}

pub fn search_stats(needs: &crate::search_query::Needs) -> SearchStats {
    match with_connection(|conn| query_search_stats(conn, needs)) {
        Ok(stats) => stats,
        Err(e) => {
            warn!("Unable to load the play stats for a search: {e}");
            SearchStats::default()
        }
    }
}

fn query_search_stats(
    conn: &Connection,
    needs: &crate::search_query::Needs,
) -> rusqlite::Result<SearchStats> {
    let mut stats = SearchStats::default();
    if needs.stats {
        let mut stmt = conn.prepare("SELECT uri, plays, last_played FROM totals")?;
        for row in stmt.query_map([], |row| {
            let track = TrackStats {
                plays: row.get(1)?,
                last_heard: row.get(2)?,
            };
            Ok((row.get(0)?, track))
        })? {
            let (uri, track) = row?;
            stats.heard.insert(uri, track);
        }
    }
    if needs.favorites {
        let mut stmt = conn.prepare("SELECT uri, added FROM favorites")?;
        stats.favorites = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
    }
    if needs.favorite_albums {
        let mut stmt = conn.prepare("SELECT album_id FROM favorite_albums")?;
        stats.favorite_albums = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
    }
    if needs.favorite_works {
        let mut stmt = conn.prepare(
            "SELECT key FROM favorite_works UNION SELECT work_key FROM favorite_recordings",
        )?;
        stats.favorite_works = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(stats)
}

// The keys of the works that are favorites, or have a favorite recording,
// which the Works page marks
pub fn favorite_work_keys() -> std::collections::HashSet<String> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT key FROM favorite_works UNION SELECT work_key FROM favorite_recordings",
        )?;
        stmt.query_map([], |row| row.get(0))?.collect()
    })
    .unwrap_or_else(|e| {
        warn!("Unable to load the favorite works: {e}");
        Default::default()
    })
}

pub fn favorite_items() -> Vec<FavoriteItem> {
    with_connection(|conn| query_favorite_items(conn)).unwrap_or_else(|e| {
        warn!("Unable to load the favorite works: {e}");
        Vec::new()
    })
}

fn query_favorite_items(conn: &Connection) -> rusqlite::Result<Vec<FavoriteItem>> {
    let mut stmt = conn.prepare(
        "SELECT work_key, album_id, uri, added FROM favorite_recordings
         UNION ALL
         SELECT key, NULL, NULL, added FROM favorite_works
         WHERE key NOT IN (SELECT work_key FROM favorite_recordings)
         ORDER BY 4, 1",
    )?;
    stmt.query_map([], |row| {
        let work_key: String = row.get(0)?;
        let recording: Option<(String, String)> = row.get::<_, Option<String>>(1)?.zip(row.get(2)?);
        Ok(match recording {
            Some((album_id, uri)) => FavoriteItem::Recording(FavoriteRecording {
                work_key,
                album_id,
                uri,
            }),
            None => FavoriteItem::Work(work_key),
        })
    })?
    .collect()
}

// An album marked as a favorite, and the URI of its first track, once it's
// been looked up in the library
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavoriteAlbum {
    pub id: String,
    pub uri: Option<String>,
}

// The favorite albums are read and written on the main thread too, like
// the favorite tracks. Albums marked together, from a selection, keep the
// order they're given in
pub fn set_favorite_albums(ids: &[String], favorite: bool) {
    let result = with_connection(|conn| set_albums(conn, ids, favorite, now()));
    if let Err(e) = result {
        warn!("Unable to change whether albums are favorites: {e}");
    }
}

// Marking one again keeps its place in the list, and its first track
fn set_albums(
    conn: &mut Connection,
    ids: &[String],
    favorite: bool,
    added: i64,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    for id in ids {
        if favorite {
            tx.execute(
                "INSERT OR IGNORE INTO favorite_albums (album_id, added) VALUES (?1, ?2)",
                params![id, added],
            )?;
        } else {
            tx.execute("DELETE FROM favorite_albums WHERE album_id = ?1", [id])?;
        }
    }
    tx.commit()
}

// The ids of the favorite albums, in a set whose order doesn't change
pub fn favorite_album_ids() -> BTreeSet<String> {
    with_connection(|conn| {
        let mut stmt = conn.prepare("SELECT album_id FROM favorite_albums")?;
        stmt.query_map([], |row| row.get(0))?.collect()
    })
    .unwrap_or_else(|e| {
        warn!("Unable to load the favorite albums: {e}");
        Default::default()
    })
}

// The favorite albums, in the order they were marked
pub fn favorite_albums() -> Vec<FavoriteAlbum> {
    with_connection(|conn| query_favorite_albums(conn)).unwrap_or_else(|e| {
        warn!("Unable to load the favorite albums: {e}");
        Vec::new()
    })
}

fn query_favorite_albums(conn: &Connection) -> rusqlite::Result<Vec<FavoriteAlbum>> {
    let mut stmt =
        conn.prepare("SELECT album_id, uri FROM favorite_albums ORDER BY added, rowid")?;
    stmt.query_map([], |row| {
        Ok(FavoriteAlbum {
            id: row.get(0)?,
            uri: row.get(1)?,
        })
    })?
    .collect()
}

// What follows the favorite albums through changes to the library: the
// albums whose ids changed, as (old id, new id), and the URIs of the first
// tracks of the albums looked up since they were marked, as (id, URI)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FavoriteAlbumUpdates {
    pub moves: Vec<(String, String)>,
    pub uris: Vec<(String, String)>,
}

// Follows the favorite albums to their new ids, and keeps the URIs of their
// first tracks. An album unmarked meanwhile stays unmarked, and one that
// moves onto a favorite stays in that one's place
pub fn update_favorite_albums(updates: &FavoriteAlbumUpdates) {
    if updates.moves.is_empty() && updates.uris.is_empty() {
        return;
    }
    let result = with_connection(|conn| update_albums(conn, updates));
    if let Err(e) = result {
        warn!("Unable to update the favorite albums: {e}");
    }
}

fn update_albums(conn: &mut Connection, updates: &FavoriteAlbumUpdates) -> rusqlite::Result<()> {
    let FavoriteAlbumUpdates { moves, uris } = updates;
    let tx = conn.transaction()?;
    for (old, new) in moves {
        tx.execute(
            "UPDATE OR IGNORE favorite_albums SET album_id = ?2 WHERE album_id = ?1",
            [old, new],
        )?;
        tx.execute("DELETE FROM favorite_albums WHERE album_id = ?1", [old])?;
    }
    for (id, uri) in uris {
        tx.execute(
            "UPDATE favorite_albums SET uri = ?2 WHERE album_id = ?1 AND uri IS NULL",
            [id, uri],
        )?;
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn
    }

    fn listen(uri: &str, ended: i64, heard: u64) -> Listen {
        Listen {
            uri: uri.to_string(),
            ended,
            heard,
            counted: false,
        }
    }

    // Positions as the player reports them, four times a second
    fn play(tracker: &mut ListenTracker, from: u64, to: u64, playing: bool) {
        for quarter in from * 4..=to * 4 {
            tracker.advance(quarter / 4, playing);
        }
    }

    fn heard(tracker: &mut ListenTracker) -> Option<u64> {
        tracker.finish(0).map(|listen| listen.heard)
    }

    #[test]
    fn steady_playback_is_heard() {
        let mut tracker = ListenTracker::default();
        tracker.start("file:///a.opus".to_string(), 300);
        tracker.advance(0, false);
        play(&mut tracker, 0, 100, true);
        assert_eq!(heard(&mut tracker), Some(100));
    }

    #[test]
    fn a_late_position_is_still_heard() {
        let mut tracker = ListenTracker::default();
        tracker.start("file:///a.opus".to_string(), 300);
        play(&mut tracker, 0, 40, true);
        play(&mut tracker, 42, 60, true);
        assert_eq!(heard(&mut tracker), Some(60));
    }

    #[test]
    fn jumps_and_pauses_are_not_heard() {
        let mut tracker = ListenTracker::default();
        tracker.start("file:///a.opus".to_string(), 600);
        play(&mut tracker, 0, 40, true);
        // A seek forward
        play(&mut tracker, 100, 120, true);
        // Paused
        play(&mut tracker, 120, 150, false);
        // Going back to the start
        play(&mut tracker, 0, 10, true);
        assert_eq!(heard(&mut tracker), Some(70));
    }

    #[test]
    fn a_resume_point_is_not_heard() {
        let mut tracker = ListenTracker::default();
        tracker.start("file:///a.opus".to_string(), 600);
        tracker.advance(0, false);
        tracker.advance(200, false);
        play(&mut tracker, 200, 240, true);
        assert_eq!(heard(&mut tracker), Some(40));
    }

    #[test]
    fn nothing_is_heard_without_a_track() {
        let mut tracker = ListenTracker::default();
        play(&mut tracker, 0, 100, true);
        assert_eq!(heard(&mut tracker), None);

        tracker.start("file:///a.opus".to_string(), 300);
        play(&mut tracker, 0, 100, true);
        tracker.clear();
        play(&mut tracker, 100, 200, true);
        assert_eq!(heard(&mut tracker), None);
    }

    #[test]
    fn playing_a_track_again_is_another_listen() {
        let mut tracker = ListenTracker::default();
        tracker.start("file:///a.opus".to_string(), 60);
        play(&mut tracker, 0, 60, true);
        assert_eq!(heard(&mut tracker), Some(60));
        assert_eq!(heard(&mut tracker), None);

        play(&mut tracker, 0, 45, true);
        assert_eq!(heard(&mut tracker), Some(45));
    }

    #[test]
    fn long_tracks_need_30_seconds_and_4_minutes() {
        let mut tracker = ListenTracker::default();
        let mut listen_after = |seconds| {
            tracker.start("file:///a.opus".to_string(), 1200);
            play(&mut tracker, 0, seconds, true);
            tracker.finish(0).map(|listen| listen.counted)
        };
        assert_eq!(listen_after(29), None);
        assert_eq!(listen_after(30), Some(false));
        assert_eq!(listen_after(239), Some(false));
        assert_eq!(listen_after(240), Some(true));
    }

    #[test]
    fn short_tracks_need_half_of_them() {
        let mut tracker = ListenTracker::default();
        let mut listen_after = |seconds| {
            tracker.start("file:///a.opus".to_string(), 21);
            play(&mut tracker, 0, seconds, true);
            tracker.finish(0).map(|listen| listen.counted)
        };
        assert_eq!(listen_after(10), None);
        assert_eq!(listen_after(11), Some(true));
    }

    #[test]
    fn mid_length_tracks_count_at_half() {
        let mut tracker = ListenTracker::default();
        let mut listen_after = |seconds| {
            tracker.start("file:///a.opus".to_string(), 180);
            play(&mut tracker, 0, seconds, true);
            tracker.finish(0).map(|listen| listen.counted)
        };
        assert_eq!(listen_after(29), None);
        assert_eq!(listen_after(30), Some(false));
        assert_eq!(listen_after(90), Some(true));
    }

    #[test]
    fn tracks_of_unknown_length_need_30_seconds_and_4_minutes() {
        let mut tracker = ListenTracker::default();
        let mut listen_after = |seconds| {
            tracker.start("file:///a.opus".to_string(), 0);
            play(&mut tracker, 0, seconds, true);
            tracker.finish(0).map(|listen| listen.counted)
        };
        assert_eq!(listen_after(29), None);
        assert_eq!(listen_after(30), Some(false));
        assert_eq!(listen_after(240), Some(true));
    }

    #[test]
    fn recently_played_is_in_the_order_last_heard_and_once_each() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        insert_listen(&mut conn, &listen("a", now - 300, 60)).unwrap();
        insert_listen(&mut conn, &listen("b", now - 200, 60)).unwrap();
        insert_listen(&mut conn, &listen("a", now - 100, 60)).unwrap();
        // Ended in the same second, but logged after
        insert_listen(&mut conn, &listen("c", now - 100, 60)).unwrap();
        assert_eq!(query_recently_played(&conn).unwrap(), ["b", "a", "c"]);
    }

    #[test]
    fn recently_played_keeps_the_latest_tracks() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        let size = SMART_PLAYLIST_SIZE;
        for i in 0..=size {
            insert_listen(&mut conn, &listen(&i.to_string(), now - size + i, 60)).unwrap();
        }
        let expected: Vec<String> = (1..=size).map(|i| i.to_string()).collect();
        assert_eq!(query_recently_played(&conn).unwrap(), expected);
    }

    fn played(uri: &str, ended: i64) -> Listen {
        Listen {
            counted: true,
            ..listen(uri, ended, 600)
        }
    }

    #[test]
    fn most_played_ranks_by_plays_in_the_last_6_months() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        insert_listen(&mut conn, &played("song", now - 3 * DAY)).unwrap();
        insert_listen(&mut conn, &played("song", now - 2 * DAY)).unwrap();
        insert_listen(&mut conn, &played("movement", now - DAY)).unwrap();
        // Heard, but not long enough to be a play
        insert_listen(&mut conn, &listen("skipped", now - DAY, 3000)).unwrap();
        // Played the most, but too long ago
        for days in 200..203 {
            insert_listen(&mut conn, &played("old", now - days * DAY)).unwrap();
        }
        assert_eq!(query_most_played(&conn, now).unwrap(), ["song", "movement"]);
    }

    #[test]
    fn a_work_played_as_often_keeps_its_order() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        insert_listen(&mut conn, &played("favorite", now - 2 * DAY)).unwrap();
        for (i, movement) in ["I", "II", "III", "IV"].iter().enumerate() {
            insert_listen(&mut conn, &played(movement, now - DAY + i as i64 * 600)).unwrap();
        }
        insert_listen(&mut conn, &played("favorite", now)).unwrap();
        assert_eq!(
            query_most_played(&conn, now).unwrap(),
            ["favorite", "I", "II", "III", "IV"]
        );
    }

    #[test]
    fn most_played_keeps_the_latest_of_as_many_plays() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        let size = SMART_PLAYLIST_SIZE;
        for i in 0..=size {
            insert_listen(&mut conn, &played(&i.to_string(), now - size + i)).unwrap();
        }
        let expected: Vec<String> = (1..=size).map(|i| i.to_string()).collect();
        assert_eq!(query_most_played(&conn, now).unwrap(), expected);
    }

    #[test]
    fn not_heard_lately_is_the_most_played_not_heard_in_2_months() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        let long_ago = now - 300 * DAY;
        for i in 0..3 {
            insert_listen(&mut conn, &played("three plays", long_ago + i)).unwrap();
        }
        for i in 0..5 {
            insert_listen(&mut conn, &played("five plays", long_ago + i)).unwrap();
        }
        // Heard lately, if only for less than a play
        for i in 0..8 {
            insert_listen(&mut conn, &played("heard lately", long_ago + i)).unwrap();
        }
        insert_listen(&mut conn, &listen("heard lately", now - 10 * DAY, 60)).unwrap();
        // Not played often enough
        for i in 0..2 {
            insert_listen(&mut conn, &played("two plays", long_ago + i)).unwrap();
        }
        // Played often more than a year ago, which only the totals keep
        for i in 0..4 {
            insert_listen(&mut conn, &played("year ago", now - 400 * DAY + i)).unwrap();
        }
        assert_eq!(
            query_not_heard_lately(&conn, now).unwrap(),
            ["five plays", "year ago", "three plays"]
        );
    }

    #[test]
    fn not_heard_lately_is_expressible_in_the_search_syntax() {
        use crate::search_query::{Candidate, Context, Mode, Query};

        let mut conn = test_db();
        let now = 1_800_000_000;
        let long_ago = now - 300 * DAY;
        for (uri, plays, last) in [
            ("five plays", 5, long_ago),
            ("three plays", 3, now - 62 * DAY),
            // Just inside the 61 days, which a day's precision would miss
            ("heard 60 days ago", 9, now - 61 * DAY + 3600),
            ("two plays", 2, long_ago),
        ] {
            for i in 0..plays {
                insert_listen(&mut conn, &played(uri, last - i)).unwrap();
            }
        }
        let query = Query::parse("plays:3.. played:<-61d", Mode::Search);
        let stats = query_search_stats(&conn, &query.needs()).unwrap();
        let mut found: Vec<&str> = stats
            .heard
            .iter()
            .filter(|(_, track)| {
                let candidate = Candidate {
                    plays: Some(i64::from(track.plays)),
                    played: Some(track.last_heard),
                    ..Default::default()
                };
                query.matches(&candidate, &Context { now })
            })
            .map(|(uri, _)| uri.as_str())
            .collect();
        found.sort_unstable();
        let mut expected = query_not_heard_lately(&conn, now).unwrap();
        expected.sort();
        assert_eq!(found, expected);
        assert_eq!(found, ["five plays", "three plays"]);
    }

    #[test]
    fn a_work_not_heard_lately_keeps_its_order() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        for play in 0..3 {
            for (i, movement) in ["I", "II", "III"].iter().enumerate() {
                let ended = now - (100 - play) * DAY + i as i64 * 600;
                insert_listen(&mut conn, &played(movement, ended)).unwrap();
            }
        }
        assert_eq!(
            query_not_heard_lately(&conn, now).unwrap(),
            ["I", "II", "III"]
        );
    }

    #[test]
    fn listens_older_than_a_year_are_dropped_but_not_their_totals() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        let mut old = listen("a", now - 400 * DAY, 100);
        old.counted = true;
        insert_listen(&mut conn, &old).unwrap();
        insert_listen(&mut conn, &listen("a", now, 50)).unwrap();

        let ended: Vec<i64> = conn
            .prepare("SELECT ended FROM listens")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ended, [now]);

        let totals: (i64, i64, i64) = conn
            .query_row(
                "SELECT plays, heard, last_played FROM totals WHERE uri = 'a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(totals, (1, 150, now));
    }

    #[test]
    fn the_last_played_time_does_not_go_back() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        insert_listen(&mut conn, &listen("a", now, 60)).unwrap();
        insert_listen(&mut conn, &listen("a", now - 60, 60)).unwrap();
        let last_played: i64 = conn
            .query_row("SELECT last_played FROM totals", [], |row| row.get(0))
            .unwrap();
        assert_eq!(last_played, now);
    }

    #[test]
    fn track_stats_are_the_totals_of_the_tracks_heard() {
        let mut conn = test_db();
        let now = 1_800_000_000;
        insert_listen(&mut conn, &played("a", now - DAY)).unwrap();
        insert_listen(&mut conn, &played("a", now - 2 * DAY)).unwrap();
        // Heard since, but not long enough to be a play
        insert_listen(&mut conn, &listen("a", now, 60)).unwrap();
        insert_listen(&mut conn, &listen("b", now - 3 * DAY, 60)).unwrap();

        let uris = ["a", "b", "never heard"].map(String::from);
        let stats = query_track_stats(&mut conn, &uris).unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(
            stats["a"],
            TrackStats {
                plays: 2,
                last_heard: now
            }
        );
        assert_eq!(
            stats["b"],
            TrackStats {
                plays: 0,
                last_heard: now - 3 * DAY
            }
        );
    }

    #[test]
    fn a_newer_schema_is_left_as_it_is() {
        let mut conn = test_db();
        insert_listen(&mut conn, &listen("a", 1_800_000_000, 60)).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();

        migrate(&mut conn).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
        assert_eq!(query_recently_played(&conn).unwrap(), ["a"]);
    }

    #[test]
    fn migrating_again_keeps_the_listens() {
        let mut conn = test_db();
        insert_listen(&mut conn, &listen("a", 1_800_000_000, 60)).unwrap();
        conn.pragma_update(None, "user_version", 0).unwrap();

        migrate(&mut conn).unwrap();
        assert_eq!(query_recently_played(&conn).unwrap(), ["a"]);
    }

    #[test]
    fn version_1_gains_the_favorites_and_keeps_the_listens() {
        let mut conn = test_db();
        insert_listen(&mut conn, &listen("a", 1_800_000_000, 60)).unwrap();
        conn.execute_batch("DROP TABLE favorites").unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();

        migrate(&mut conn).unwrap();
        insert_favorite(&conn, "a", 1_800_000_000).unwrap();
        assert_eq!(query_favorites(&conn).unwrap(), ["a"]);
        assert_eq!(query_recently_played(&conn).unwrap(), ["a"]);
    }

    #[test]
    fn version_2_gains_the_favorite_works_and_keeps_the_favorites() {
        let mut conn = test_db();
        insert_favorite(&conn, "file:///music/a.flac", 1).unwrap();
        conn.execute_batch(
            "DROP TABLE favorite_works; DROP TABLE favorite_recordings;
             PRAGMA user_version = 2;",
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        set_work(&conn, "work", true, 2).unwrap();
        assert_eq!(query_favorites(&conn).unwrap(), ["file:///music/a.flac"]);
        assert_eq!(
            query_favorite_items(&conn).unwrap(),
            [FavoriteItem::Work("work".to_string())]
        );
    }

    #[test]
    fn version_3_gains_the_favorite_albums_and_keeps_the_other_favorites() {
        let mut conn = test_db();
        insert_favorite(&conn, "file:///music/a.flac", 1).unwrap();
        set_work(&conn, "work", true, 2).unwrap();
        conn.execute_batch(
            "DROP TABLE favorite_albums;
             PRAGMA user_version = 3;",
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        set_albums(&mut conn, &["album".to_string()], true, 3).unwrap();
        assert_eq!(query_favorites(&conn).unwrap(), ["file:///music/a.flac"]);
        assert_eq!(
            query_favorite_items(&conn).unwrap(),
            [FavoriteItem::Work("work".to_string())]
        );
        assert_eq!(
            query_favorite_albums(&conn).unwrap(),
            [FavoriteAlbum {
                id: "album".to_string(),
                uri: None
            }]
        );
    }

    fn album_ids(conn: &Connection) -> Vec<String> {
        query_favorite_albums(conn)
            .unwrap()
            .into_iter()
            .map(|album| album.id)
            .collect()
    }

    #[test]
    fn favorite_albums_are_in_the_order_they_were_marked() {
        let mut conn = test_db();
        let ids = |ids: &[&str]| ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        set_albums(&mut conn, &ids(&["c"]), true, 10).unwrap();
        // Marked together, in the order of the grid
        set_albums(&mut conn, &ids(&["b", "a"]), true, 20).unwrap();
        // Marking one again keeps its place
        set_albums(&mut conn, &ids(&["c"]), true, 30).unwrap();
        assert_eq!(album_ids(&conn), ["c", "b", "a"]);

        set_albums(&mut conn, &ids(&["b", "c"]), false, 0).unwrap();
        assert_eq!(album_ids(&conn), ["a"]);
    }

    #[test]
    fn favorite_albums_follow_their_new_ids() {
        let mut conn = test_db();
        let ids = |ids: &[&str]| ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        set_albums(&mut conn, &ids(&["a", "b", "c"]), true, 10).unwrap();
        let uris = |uris: Vec<(String, String)>| FavoriteAlbumUpdates {
            uris,
            ..Default::default()
        };
        update_albums(
            &mut conn,
            &uris(vec![
                pair("a", "file:///music/a/01.flac"),
                pair("b", "file:///b"),
            ]),
        )
        .unwrap();
        // A URI looked up again doesn't replace the one kept
        update_albums(&mut conn, &uris(vec![pair("a", "file:///music/a/02.flac")])).unwrap();

        // "a" is now "x", and "c" is now "b", which is a favorite already
        let moves = FavoriteAlbumUpdates {
            moves: vec![pair("a", "x"), pair("c", "b")],
            ..Default::default()
        };
        update_albums(&mut conn, &moves).unwrap();
        assert_eq!(
            query_favorite_albums(&conn).unwrap(),
            [
                FavoriteAlbum {
                    id: "x".to_string(),
                    uri: Some("file:///music/a/01.flac".to_string())
                },
                FavoriteAlbum {
                    id: "b".to_string(),
                    uri: Some("file:///b".to_string())
                },
            ]
        );

        // Unmarked before its URI was kept, it stays unmarked
        set_albums(&mut conn, &ids(&["b"]), false, 0).unwrap();
        update_albums(&mut conn, &uris(vec![pair("b", "file:///b")])).unwrap();
        assert_eq!(album_ids(&conn), ["x"]);
    }

    #[test]
    fn favorite_works_and_recordings_are_in_the_order_they_were_marked() {
        let conn = test_db();
        let recording = |work_key: &str, uri: &str| FavoriteRecording {
            work_key: work_key.to_string(),
            album_id: "album".to_string(),
            uri: uri.to_string(),
        };
        set_work(&conn, "a", true, 10).unwrap();
        set_recording(&conn, &recording("b", "file:///music/b1.flac"), true, 20).unwrap();
        // A work with a favorite recording plays that, so it comes once
        set_work(&conn, "b", true, 30).unwrap();
        set_work(&conn, "c", true, 40).unwrap();
        // Marking one again keeps its place
        set_work(&conn, "a", true, 50).unwrap();
        assert_eq!(
            query_favorite_items(&conn).unwrap(),
            [
                FavoriteItem::Work("a".to_string()),
                FavoriteItem::Recording(recording("b", "file:///music/b1.flac")),
                FavoriteItem::Work("c".to_string()),
            ]
        );

        // Unmarking the recording leaves the work, at its own place
        set_recording(&conn, &recording("b", "file:///music/b1.flac"), false, 0).unwrap();
        set_work(&conn, "c", false, 0).unwrap();
        assert_eq!(
            query_favorite_items(&conn).unwrap(),
            [
                FavoriteItem::Work("a".to_string()),
                FavoriteItem::Work("b".to_string()),
            ]
        );
    }

    #[test]
    fn a_favorite_can_be_marked_and_unmarked() {
        let conn = test_db();
        assert!(!query_is_favorite(&conn, "a").unwrap());
        insert_favorite(&conn, "a", 1_800_000_000).unwrap();
        assert!(query_is_favorite(&conn, "a").unwrap());
        assert!(!query_is_favorite(&conn, "b").unwrap());
        delete_favorite(&conn, "a").unwrap();
        assert!(!query_is_favorite(&conn, "a").unwrap());
    }

    #[test]
    fn favorites_are_in_the_order_they_were_marked() {
        let conn = test_db();
        let now = 1_800_000_000;
        insert_favorite(&conn, "c", now - 100).unwrap();
        insert_favorite(&conn, "a", now).unwrap();
        // Marked in the same second, but after
        insert_favorite(&conn, "b", now).unwrap();
        assert_eq!(query_favorites(&conn).unwrap(), ["c", "a", "b"]);
    }

    #[test]
    fn marking_a_favorite_again_keeps_its_place() {
        let conn = test_db();
        let now = 1_800_000_000;
        insert_favorite(&conn, "a", now - 100).unwrap();
        insert_favorite(&conn, "b", now - 50).unwrap();
        insert_favorite(&conn, "a", now).unwrap();
        assert_eq!(query_favorites(&conn).unwrap(), ["a", "b"]);

        // Unmarked first, it goes to the end
        delete_favorite(&conn, "a").unwrap();
        insert_favorite(&conn, "a", now).unwrap();
        assert_eq!(query_favorites(&conn).unwrap(), ["b", "a"]);
    }

    #[test]
    fn clearing_the_history_keeps_the_favorites() {
        let mut conn = test_db();
        let now = now();
        insert_listen(&mut conn, &listen("a", now - 100, 60)).unwrap();
        insert_listen(&mut conn, &listen("b", now - 50, 60)).unwrap();
        insert_favorite(&conn, "a", now).unwrap();
        set_albums(&mut conn, &["album".to_string()], true, now).unwrap();

        delete_history(&mut conn).unwrap();
        assert!(query_recently_played(&conn).unwrap().is_empty());
        let uris = ["a".to_string(), "b".to_string()];
        assert!(query_track_stats(&mut conn, &uris).unwrap().is_empty());
        assert_eq!(query_favorites(&conn).unwrap(), ["a"]);
        assert_eq!(album_ids(&conn), ["album"]);

        // Listening again starts it over
        insert_listen(&mut conn, &listen("b", now, 60)).unwrap();
        assert_eq!(query_recently_played(&conn).unwrap(), ["b"]);
    }
}
