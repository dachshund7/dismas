// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// What the queue's search takes from the library for each track it has,
// by path: when it was added, and its album and work, for `added:`,
// `is:favorite-album` and `is:favorite-work`
#[derive(Clone, Debug, Default)]
pub(crate) struct TrackEntry {
    pub(crate) added: i64,
    pub(crate) album_id: Option<String>,
    pub(crate) work_key: Option<String>,
}

pub(crate) fn track_entries() -> HashMap<PathBuf, TrackEntry> {
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

// What the tracks taken from albums match: a search in the search syntax,
// matched as in the queue (`dismas_match()`), a composer and a performer, by
// their keys (`composer_key()`), a work, by its key (see `work_entry()`) and
// those of the arrangements listed under it, tracks by their paths, as
// the cover of a recording of a work takes its tracks, and the favorites.
// An empty one matches every
// track. Two filters made from the same search and picks are equal, as the
// time its relative dates go by is only taken when it's matched
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrackFilter {
    // The search, unless it matches every track, see `TrackFilter::query()`
    pub(crate) query: Option<Arc<Query>>,
    // What the search needs from the window, if anything
    pub(crate) sources: Option<Arc<SearchSources>>,
    pub(crate) composer: Option<String>,
    pub(crate) performer: Option<String>,
    pub(crate) works: Vec<String>,
    // The tracks themselves, by their paths in the library; none matches an
    // empty list
    pub(crate) tracks: Option<Arc<[PathBuf]>>,
    pub(crate) favorites: Option<Favorites>,
}

// The favorites that the tracks have to be among. An empty list matches no
// track, as there are no favorites to show. Each is a value bound to the
// statement, and SQLite takes up to 32,766 of them, far more favorites
// than anyone marks; past that, the search fails and finds nothing
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Favorites {
    // The tracks of the favorite albums, by their ids
    Albums(Arc<[String]>),
    // The favorite tracks, by their paths in the library
    Tracks(Arc<[PathBuf]>),
}

// What a search needs that the library doesn't keep
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SearchSources {
    // The music folders that came through the document portal, with their
    // paths outside the sandbox, which `path:` looks into
    pub(crate) shown_folders: Vec<(PathBuf, PathBuf)>,
}

// What `dismas_match()` matches a track with, bound to each statement, so
// that each has its own search, and a statement without one fails rather
// than matching with another's
pub(super) struct Matcher {
    // With its relative dates worked out at `context`, see `Query::at()`
    query: Query,
    context: Context,
    stats: Arc<PathStats>,
    shown_folders: Vec<(PathBuf, PathBuf)>,
}

// The play stats and the favorites that a search reads from plays.db, by
// path rather than by URI, as the library keeps the tracks, whose paths
// are those of their URIs, even for the tracks of CUE sheets
#[derive(Debug, Default)]
struct PathStats {
    heard: HashMap<PathBuf, play_stats::TrackStats>,
    favorites: HashMap<PathBuf, i64>,
    favorite_albums: BTreeSet<String>,
    favorite_works: HashSet<String>,
}

impl From<play_stats::SearchStats> for PathStats {
    fn from(stats: play_stats::SearchStats) -> Self {
        let by_path = |uri: &String| gio::File::for_uri(uri).path();
        Self {
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
        }
    }
}

// What a search reads from plays.db, in the thread of its statement. A key
// typed in the album search makes several statements, each with its
// `Matcher` (the albums, the works, the pickers), in threads of their own,
// and each read the stats of every track heard and turned their URIs into
// paths: so the ones read last are kept, with what they were read for,
// until plays.db is written to. Statements that come at the same time wait
// for the one that reads them. Tests give their own, and never read the
// user's
#[cfg(not(test))]
fn search_stats(needs: &Needs) -> Arc<PathStats> {
    type Read = ([bool; 4], u64, Arc<PathStats>);
    static LAST: std::sync::Mutex<Option<Read>> = std::sync::Mutex::new(None);
    let wanted = [
        needs.stats,
        needs.favorites,
        needs.favorite_albums,
        needs.favorite_works,
    ];
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let writes = play_stats::writes();
    if let Some((read_for, read_at, stats)) = &*last
        && *read_for == wanted
        && *read_at == writes
    {
        return Arc::clone(stats);
    }
    let stats = Arc::new(PathStats::from(play_stats::search_stats(needs)));
    *last = Some((wanted, writes, Arc::clone(&stats)));
    stats
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_SEARCH_STATS: RefCell<play_stats::SearchStats> = RefCell::default();
}

#[cfg(test)]
fn search_stats(_needs: &Needs) -> Arc<PathStats> {
    Arc::new(TEST_SEARCH_STATS.with_borrow(Clone::clone).into())
}

impl Matcher {
    fn new(query: &Arc<Query>, sources: Option<&SearchSources>) -> Self {
        let context = Context::now();
        Self {
            query: query.at(&context),
            context,
            stats: search_stats(&query.needs()),
            shown_folders: sources
                .map(|sources| sources.shown_folders.clone())
                .unwrap_or_default(),
        }
    }
}

// The type of the pointer to a `Matcher` that statements are given
const MATCHER: &std::ffi::CStr = c"dismas-matcher";

// A value bound to a statement: a plain one, or the search it matches with
pub(super) enum Param {
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
pub(super) fn add_functions(conn: &Connection) -> rusqlite::Result<()> {
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
                .and_then(|path| matcher.stats.favorites.get(path.as_ref()).copied());
            let heard = path
                .as_ref()
                .and_then(|path| matcher.stats.heard.get(path.as_ref()));
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
                favorite_album: text(5)
                    .is_some_and(|id| matcher.stats.favorite_albums.contains(id)),
                favorite_work: text(6)
                    .is_some_and(|key| matcher.stats.favorite_works.contains(key)),
            };
            Ok(matcher.query.matches(&candidate, &matcher.context))
        },
    )
}

impl TrackFilter {
    // The search of a text, unless it matches every track
    pub(crate) fn query(text: &str) -> Option<Arc<Query>> {
        Some(Query::parse(text, Mode::Search))
            .filter(|query| !query.matches_all())
            .map(Arc::new)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.query.is_none()
            && self.composer.is_none()
            && self.performer.is_none()
            && self.works.is_empty()
            && self.tracks.is_none()
            && self.favorites.is_none()
    }

    // The SQL conditions on a track, each after " AND ", and their values;
    // a track without search text, composers, performers or a work matches
    // nothing
    pub(super) fn conditions(&self) -> (String, Vec<Param>) {
        self.conditions_with(true)
    }

    // The conditions alone, without reading what the values need
    pub(super) fn conditions_sql(&self) -> String {
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
        // The tracks of a list of paths, and none for an empty one
        let mut in_paths = |paths: &[PathBuf]| {
            if paths.is_empty() {
                sql.push_str(" AND 0");
                return;
            }
            sql.push_str(&format!(" AND path IN ({})", placeholders(paths.len())));
            values.extend(
                paths
                    .iter()
                    .map(|path| Value::Blob(path_bytes(path).to_vec()).into()),
            );
        };
        if let Some(paths) = &self.tracks {
            in_paths(paths);
        }
        match &self.favorites {
            Some(Favorites::Tracks(paths)) => in_paths(paths),
            Some(Favorites::Albums(ids)) if ids.is_empty() => sql.push_str(" AND 0"),
            Some(Favorites::Albums(ids)) => {
                sql.push_str(&format!(" AND album_id IN ({})", placeholders(ids.len())));
                values.extend(ids.iter().cloned().map(|id| Value::Text(id).into()));
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
    pub(super) fn matches_sql(&self) -> String {
        format!("(CASE WHEN 1{} THEN 1 ELSE 0 END)", self.conditions_sql())
    }
}

// The values of a statement that has the filter's `matches_sql()` before
// the condition on the `hidden` folders, see `hidden_condition()`
pub(super) fn filter_values(filter: &TrackFilter, hidden: &[PathBuf]) -> Vec<Param> {
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
