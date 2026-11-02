// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Whether a track matches a query: the one evaluator, for the queue and
//! for the library

use std::{cell::OnceCell, path::Path};

use super::{Field, IsFlag, Node, Source, Term, Test, values};
use crate::audio::{FieldCode, field_values, normalize_search_text};

// What a query looks at in a track, borrowed from wherever the track keeps
// it; `Query::needs()` says which parts a query needs
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Candidate<'a> {
    // The text of the tags, folded, see `search_key()`
    pub(crate) search_key: &'a str,
    // The fields, see `FieldParts`
    pub(crate) fields: &'a str,
    // The path that's shown, outside the sandbox
    pub(crate) path: Option<&'a Path>,
    pub(crate) favorite: bool,
    // Whether its file is gone, in the queue
    pub(crate) missing: bool,
    // In seconds since the epoch: when the track was added to the library,
    // when it was last heard, and when it was marked a favorite
    pub(crate) added: Option<i64>,
    pub(crate) played: Option<i64>,
    pub(crate) favorited: Option<i64>,
    // How many times it was played, 0 for a track never heard
    pub(crate) plays: Option<i64>,
    pub(crate) favorite_album: bool,
    pub(crate) favorite_work: bool,
}

// When the query is matched, for the relative dates
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    // In seconds since the epoch
    pub(crate) now: i64,
}

impl Context {
    pub(crate) fn now() -> Self {
        Self {
            now: gtk::glib::real_time() / 1_000_000,
        }
    }
}

// A track being matched, with its path folded once for a whole query
struct Track<'a> {
    candidate: &'a Candidate<'a>,
    path: OnceCell<Option<String>>,
    now: i64,
}

impl<'a> Track<'a> {
    fn values(&self, codes: &'static [FieldCode]) -> impl Iterator<Item = &'a str> {
        field_values(self.candidate.fields, codes)
    }

    fn path(&self) -> Option<&str> {
        self.path
            .get_or_init(|| {
                self.candidate
                    .path
                    .map(|path| normalize_search_text(&path.to_string_lossy()))
            })
            .as_deref()
    }
}

pub(super) fn matches(node: &Node, candidate: &Candidate, context: &Context) -> bool {
    let track = Track {
        candidate,
        path: OnceCell::new(),
        now: context.now,
    };
    eval(node, &track)
}

fn eval(node: &Node, track: &Track) -> bool {
    match node {
        Node::All => true,
        Node::Word(word) => track.candidate.search_key.contains(word.as_str()),
        Node::Term(term) => eval_term(term, track),
        Node::Has(field) => match field.source() {
            Source::Tags(codes) => track.values(codes).next().is_some(),
            Source::Path => track.candidate.path.is_some(),
            // Played at least once
            Source::Plays => track.candidate.plays.is_some_and(|plays| plays > 0),
            source => number(source, track.candidate).is_some(),
        },
        Node::Is(flag) => match flag {
            IsFlag::Favorite => track.candidate.favorite,
            IsFlag::FavoriteAlbum => track.candidate.favorite_album,
            IsFlag::FavoriteWork => track.candidate.favorite_work,
            IsFlag::Missing => track.candidate.missing,
            IsFlag::Tagged(flag) => track
                .values(&[FieldCode::Is])
                .any(|value| value == flag.name()),
        },
        Node::Not(node) => !eval(node, track),
        Node::And(nodes) => nodes.iter().all(|node| eval(node, track)),
        Node::Or(nodes) => nodes.iter().any(|node| eval(node, track)),
    }
}

// The number of a field that the `Candidate` has, which isn't in the tags,
// if the track has one
fn number(source: Source, candidate: &Candidate) -> Option<i64> {
    match source {
        Source::Added => candidate.added,
        Source::Plays => candidate.plays,
        Source::Played => candidate.played,
        Source::Favorited => candidate.favorited,
        Source::Tags(_) | Source::Path => None,
    }
}

fn eval_term(term: &Term, track: &Track) -> bool {
    let codes = match term.field.source() {
        Source::Tags(codes) => codes,
        Source::Path => {
            return match &term.test {
                Test::Path(test) => track.path().is_some_and(|path| test.matches(path)),
                _ => false,
            };
        }
        source => {
            return match (&term.test, number(source, track.candidate)) {
                (Test::Numbers(range), Some(n)) => range.contains(n),
                // A moment, rather than a period
                (Test::Dates(range), Some(time)) => range.overlaps(time, time + 1, track.now),
                _ => false,
            };
        }
    };
    let mut values = track.values(codes);
    match &term.test {
        Test::Contains(text) => values.any(|value| value.contains(text.as_str())),
        Test::Equals(text) => values.any(|value| {
            value == text
                // A performer is kept with the role, as "name (role)"
                || (term.field == Field::Performer
                    && value
                        .strip_prefix(text.as_str())
                        .is_some_and(|rest| rest.starts_with(" (") && rest.ends_with(')')))
        }),
        Test::Numbers(range) => values.any(|value| value.parse().is_ok_and(|n| range.contains(n))),
        Test::Dates(range) => values.any(|value| {
            values::tagged_span(value)
                .is_some_and(|(start, end)| range.overlaps(start, end, track.now))
        }),
        Test::Catalogue(test) => values.any(|value| test.matches(value)),
        // Only the path has these
        Test::Path(_) => false,
    }
}
