// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Whether a track matches a query: the one evaluator, for the queue and
//! for the library

use std::{cell::OnceCell, path::Path};

use super::{Field, IsFlag, Kind, Node, Term, Test, values};
use crate::audio::{FieldCode, normalize_search_text};

// What a query looks at in a track, borrowed from wherever the track keeps
// it; `Query::needs()` says which parts a query needs
#[derive(Clone, Copy, Debug, Default)]
pub struct Candidate<'a> {
    // The text of the tags, folded, see `search_key()`
    pub search_key: &'a str,
    // The fields, see `FieldParts`
    pub fields: &'a str,
    // The path that's shown, outside the sandbox
    pub path: Option<&'a Path>,
    pub favorite: bool,
    // Whether its file is gone, in the queue
    pub missing: bool,
}

// When the query is matched, for the relative dates
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    // In seconds since the epoch
    pub now: i64,
}

impl Context {
    pub fn now() -> Self {
        Self {
            now: gtk::glib::real_time() / 1_000_000,
        }
    }
}

// A track's fields, split into lines once for a whole query
struct Track<'a> {
    candidate: &'a Candidate<'a>,
    lines: OnceCell<Vec<(&'a str, &'a str)>>,
    path: OnceCell<Option<String>>,
    now: i64,
}

impl<'a> Track<'a> {
    fn values(&self, codes: &'static [FieldCode]) -> impl Iterator<Item = &'a str> + '_ {
        let lines = self.lines.get_or_init(|| {
            self.candidate
                .fields
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .collect()
        });
        lines
            .iter()
            .filter(move |(code, _)| codes.iter().any(|field| field.code() == *code))
            .map(|(_, value)| *value)
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
        lines: OnceCell::new(),
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
        Node::Has(field) => match field.kind() {
            Kind::Path => track.candidate.path.is_some(),
            _ => track.values(field.codes()).next().is_some(),
        },
        Node::Is(flag) => match flag {
            IsFlag::Favorite => track.candidate.favorite,
            IsFlag::Missing => track.candidate.missing,
            _ => track
                .values(&[FieldCode::Is])
                .any(|value| value == flag.name()),
        },
        Node::Not(node) => !eval(node, track),
        Node::And(nodes) => nodes.iter().all(|node| eval(node, track)),
        Node::Or(nodes) => nodes.iter().any(|node| eval(node, track)),
    }
}

fn eval_term(term: &Term, track: &Track) -> bool {
    let mut values = track.values(term.field.codes());
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
        Test::Path(test) => track.path().is_some_and(|path| test.matches(path)),
    }
}
