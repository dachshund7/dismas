// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// A composer of the tracks of the albums of the grid
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerInfo {
    // See `composer_key()`
    pub(crate) key: String,
    // The name its tracks give it the most, and the name it's sorted by
    pub(crate) name: String,
    pub(crate) sort: String,
    // What the list shows: the sort name, as in "Bach, Johann Sebastian",
    // when the tags give one, and otherwise the name, as the sort name
    // worked out from it would turn a band's name around, or a name whose
    // family name is first already
    pub(crate) label: String,
    // Both names, normalized with `normalize_search_text()`, which
    // searching the list looks into
    pub(crate) search: String,
    // The number of albums, or works, with a track by the composer that
    // the filter matches, see `Counted`
    pub(crate) count: u32,
}

// What the lists of composers and performers count: the albums with a
// track that the filter matches, for the album grid, or the works, for
// the Works page, where the tracks that aren't part of a work count for
// nothing, so that picking someone listed always finds some
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Counted {
    Albums,
    Works,
}

// Whether a track of the albums of the grid has a value in `column`, e.g.
// a composer, without the tracks in the `hidden` folders
pub(super) fn query_tracks_have(
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
pub(crate) fn has_composers(hidden: &[PathBuf]) -> bool {
    query_or_default("look for composers", |conn| {
        query_tracks_have(conn, "composers", hidden)
    })
}

// The composers of the tracks of the albums of the grid that the filter
// matches, by their sort names, without the tracks in the `hidden` folders,
// with the number of the albums, or works, of those tracks
pub(crate) fn composers(
    hidden: &[PathBuf],
    filter: &TrackFilter,
    counted: Counted,
) -> Vec<ComposerInfo> {
    query_or_default("list the composers", |conn| {
        query_composers(conn, hidden, filter, counted)
    })
}

// A composer's names are the ones all their tracks give them the most,
// whatever the filter, so that they're the same in every list
pub(super) fn query_composers(
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
pub(super) fn most_common<S: Ord + Into<String>>(counts: HashMap<S, usize>) -> String {
    most_counted(counts).map(Into::into).unwrap_or_default()
}

// The same, if anything was counted
pub(super) fn most_counted<S: Ord>(counts: HashMap<S, usize>) -> Option<S> {
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
pub(crate) struct PerformerInfo {
    // See `composer_key()`
    pub(crate) key: String,
    // Whether they conduct, are a group or play, by most of their albums,
    // see `query_performers()`
    pub(crate) kind: PerformerKind,
    // The name their tracks give them the most, and the name they're
    // sorted by
    pub(crate) name: String,
    pub(crate) sort: String,
    // What they play or sing, the most common first, e.g. "piano" and
    // "harpsichord"
    pub(crate) roles: Vec<String>,
    // The names and the roles, normalized with `normalize_search_text()`,
    // which searching the list looks into, so that "piano" finds the
    // pianists
    pub(crate) search: String,
    // The number of albums, or works, with a track they perform on that
    // the filter matches, see `Counted`
    pub(crate) count: u32,
}

// Whether a track of the albums of the grid has a conductor or performer,
// without the tracks in the `hidden` folders
pub(crate) fn has_performers(hidden: &[PathBuf]) -> bool {
    query_or_default("look for performers", |conn| {
        query_tracks_have(conn, "performers", hidden)
    })
}

// The conductors, groups and players of the tracks of the albums of the
// grid that the filter matches, in that order, each by their sort names,
// without the tracks in the `hidden` folders, with the number of the
// albums of those tracks
pub(crate) fn performers(
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
pub(super) fn query_performers(
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
    // The sort names by the keys of the names, made once, as a crafted file
    // can name many composers; reversed, so that the first of the names
    // with a key gives its sort name
    let sorts: HashMap<String, &String> = if sorts.len() == names.len() {
        names
            .iter()
            .map(|name| composer_key(name))
            .zip(sorts)
            .rev()
            .collect()
    } else {
        HashMap::new()
    };
    let sort_of = |composer: &str| {
        sorts
            .get(&composer_key(composer))
            .map_or_else(|| sort_name(composer), |sort| (*sort).clone())
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
pub(super) fn performer_entries(performers: &[ListedPerformer]) -> Option<String> {
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

pub(super) fn performer_kind_from_code(code: &str) -> Option<PerformerKind> {
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
pub(super) fn composer_entries(names: &[String], sorts: &[String]) -> Option<String> {
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
pub(crate) fn composer_key(name: &str) -> String {
    normalize_key_text(name)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// The name a composer is sorted by: the sort name of their tags, or else
// one worked out from their name
pub(super) fn composer_sort(name: &str, sort: &str) -> String {
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
pub(super) fn sort_name(name: &str) -> String {
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
