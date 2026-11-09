// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// Bump when the schema or the way the tags are read changes, so that
// the whole library is read again
pub(super) const SCHEMA_VERSION: i32 = 39;

// The paths of the tracks are kept as bytes, as they are not always valid
// UTF-8. The path of a track of a CUE sheet is under its sheet, which is
// the file the track comes from, with the audio files the sheet splits;
// the times and the size are those of the files. The tracks are made again
// for each version, but not the covers, nor the times their files were
// added, see `keep_added_times()`
pub(super) const SCHEMA: &str = "DROP TABLE IF EXISTS tracks;
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
        -- The dates the albums are sorted by, see `SortDates`, written as
        -- `TagDate::sort_text()`, so that MIN() takes the earliest
        sort_date TEXT,
        original_sort_date TEXT,
        recorded_sort_date TEXT,
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
        path TEXT,
        cover_files TEXT
    );
    CREATE TABLE IF NOT EXISTS added_times (
        file BLOB PRIMARY KEY,
        added INTEGER NOT NULL
    );";

fn data_dir() -> PathBuf {
    let mut dir = glib::user_data_dir();
    dir.push("dismas");
    dir
}

// Each thread keeps its connection, rather than opening the database for
// each query: the scan has a thread of its own, and the queries of the UI
// run in the threads of GLib's pool
thread_local! {
    static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

pub(super) fn with_connection<T>(
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
pub(super) fn query_or_default<T: Default>(
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
pub(super) fn open_or_recreate(path: &Path) -> rusqlite::Result<Connection> {
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

pub(super) fn open_at(path: &Path) -> rusqlite::Result<Connection> {
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
        add_cover_files_column(&tx)?;
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

// The covers outlive the versions of the schema, so a column added to
// them since is added to the table an older version made. The albums it
// has start without the key of their cover files, which the next scan
// fills in, see `update_covers`
fn add_cover_files_column(conn: &Connection) -> rusqlite::Result<()> {
    let has_column: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('covers') WHERE name = 'cover_files')",
        [],
        |row| row.get(0),
    )?;
    if !has_column {
        conn.execute_batch("ALTER TABLE covers ADD COLUMN cover_files TEXT")?;
    }
    Ok(())
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
pub(super) fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}

pub(super) fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
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
pub(super) fn path_of_bytes(bytes: &[u8]) -> Cow<'_, Path> {
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
pub(super) fn hidden_condition(column: &str, hidden: &[PathBuf]) -> (String, Vec<Vec<u8>>) {
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

// The placeholders of `n` values in a list of SQL, e.g. "?, ?"
pub(super) fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).join(", ")
}
