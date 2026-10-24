// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! What has been played, and the favorite tracks, for the smart playlists
//!
//! Each listen to a track is logged with the seconds heard, in a database
//! of its own: the library's is a cache that is made again when its schema
//! changes, and neither the history nor the favorites can be read again
//! from anywhere. The log keeps the last year; the totals of each track
//! are kept for good.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gtk::glib;
use log::{debug, warn};
use rusqlite::{Connection, TransactionBehavior, params};

const SCHEMA_VERSION: i32 = 2;

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
    glib::mkdir_with_parents(&dir, 0o755);

    let mut conn = Connection::open(dir.join("plays.db"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrate(&mut conn)?;
    Ok(conn)
}

// Unlike the library, the tables are never made again: a later version
// changes them in place. Version 2 only adds the favorites table, as each
// statement of the schema leaves a table that's already there as it is.
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

// Logs the listen, adds it to the totals of its track, and drops the
// listens older than a year
pub fn record(listen: &Listen) {
    if let Err(e) = with_connection(|conn| insert_listen(conn, listen)) {
        warn!("Unable to record the listen to {}: {e}", listen.uri);
    }
}

fn insert_listen(conn: &mut Connection, listen: &Listen) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    tx.commit()
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
}
