// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The search syntax of the queue's search and of the album search, which
//! smart playlists made by the user will be written in too: words, fields
//! (`composer:bach`, `year:<1970`), `OR`, `-` or `NOT`, parentheses, and
//! quotes. There's one grammar, read leniently for a search box
//! (`Mode::Search`) and strictly for a rule that's saved (`Mode::Rule`),
//! so that a typo can't be saved as a search for plain words. The meaning
//! of a saved rule must never change, so the syntax later features need is
//! refused now, see `Problem`.
//!
//! - The lexer works on the raw text, and only ASCII `-`, `"`, `(` and `)`
//!   are syntax; values are folded afterwards (`normalize_search_text()`),
//!   so a pasted minus sign never negates.
//! - A query matches a track through one evaluator, `Query::matches()`,
//!   in the queue and, through a function of SQLite, in the library.
//! - A term on a field the track has no value for is false, and `-` makes
//!   it true. `field:!=x` is `-field:=x`, and `has:field` tests whether
//!   there's a value.
//! - While the text has an error, such as `year:abc`, the query matches
//!   nothing, and the problem says why.

mod eval;
mod lexer;
mod parser;
mod print;
mod values;

use std::ops::Range;

pub(crate) use eval::{Candidate, Context};
// The times a date in the tags covers, which the library compares the
// recording dates of a work's tracks by
pub(crate) use values::tagged_span;
use values::{CatalogueTest, DateRange, NumberRange, PathTest};

use crate::{
    audio::{FieldCode, Flag},
    i18n::{i18n, i18n_k},
};

// The version of the syntax that saved rules are written in, so that a
// later one can read them as they were meant
#[allow(
    dead_code,
    reason = "the smart playlists made by the user will save it"
)]
pub(crate) const SYNTAX_VERSION: u32 = 1;

// How deep parentheses can go
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    // A search box: whatever doesn't parse is searched as text
    Search,
    // A rule to save: every problem is reported, see `Problem::is_error()`
    Rule,
}

// A parsed search
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Query {
    root: Node,
    // What the root looks at, worked out once, as each track matched asks
    needs: Needs,
    directives: Directives,
    problems: Vec<Problem>,
    mode: Mode,
}

impl Query {
    pub(crate) fn parse(text: &str, mode: Mode) -> Self {
        parser::parse(text, mode)
    }

    // Whether it matches every track, as an empty search does, so that it
    // doesn't filter anything
    pub(crate) fn matches_all(&self) -> bool {
        self.root == Node::All && !self.has_errors()
    }

    // Whether it can't match a track, as while it has an error
    pub(crate) fn has_errors(&self) -> bool {
        self.problems
            .iter()
            .any(|problem| problem.is_error(self.mode))
    }

    pub(crate) fn problems(&self) -> &[Problem] {
        &self.problems
    }

    #[allow(
        dead_code,
        reason = "the smart playlists made by the user will read them"
    )]
    pub(crate) fn directives(&self) -> &Directives {
        &self.directives
    }

    // What the query looks at, so that only that is read for each track
    pub(crate) fn needs(&self) -> Needs {
        self.needs
    }

    // The query with its relative dates worked out at the time of
    // `context`, so that matching many tracks doesn't work them out for
    // each; `matches()` gives the same answers with it at that time
    pub(crate) fn at(&self, context: &Context) -> Self {
        let mut query = self.clone();
        query.root.resolve_dates(context.now);
        query
    }

    // Whether a track matches, at the time `context` gives for the relative
    // dates
    pub(crate) fn matches(&self, candidate: &Candidate, context: &Context) -> bool {
        !self.has_errors() && eval::matches(&self.root, candidate, context)
    }

    // The query written the way a saved rule keeps it, which reads back as
    // the same query, see `print`
    #[allow(
        dead_code,
        reason = "the smart playlists made by the user will save their rules with it"
    )]
    pub(crate) fn to_text(&self) -> String {
        print::query(&self.root, &self.directives)
    }
}

// What a query looks at
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Needs {
    // The text of the tags, for plain words
    pub(crate) search_key: bool,
    // The fields, see `FieldCode`
    pub(crate) fields: bool,
    pub(crate) path: bool,
    pub(crate) favorites: bool,
    // The play stats, for `plays:` and `played:`
    pub(crate) stats: bool,
    // When the track was added to the library
    pub(crate) added: bool,
    pub(crate) favorite_albums: bool,
    pub(crate) favorite_works: bool,
}

impl Needs {
    // Whether it needs what the library keeps of a track, see `TrackEntry`
    pub(crate) fn library_entry(&self) -> bool {
        self.added || self.favorite_albums || self.favorite_works
    }

    // Whether marking a favorite can change what it matches
    pub(crate) fn any_favorites(&self) -> bool {
        self.favorites || self.favorite_albums || self.favorite_works
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    // Every track, as for an empty search, or a term that isn't finished
    All,
    // A word or a quoted phrase, found anywhere in the text of the tags
    Word(String),
    Term(Term),
    // Whether a track has a value for a field
    Has(Field),
    Is(IsFlag),
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
}

impl Node {
    // What the node looks at
    fn needs(&self) -> Needs {
        let mut needs = Needs::default();
        self.add_needs(&mut needs);
        needs
    }

    fn add_needs(&self, needs: &mut Needs) {
        match self {
            Self::All => (),
            Self::Word(_) => needs.search_key = true,
            Self::Term(term) => term.field.add_needs(needs),
            Self::Has(field) => field.add_needs(needs),
            Self::Is(IsFlag::Favorite) => needs.favorites = true,
            Self::Is(IsFlag::FavoriteAlbum) => needs.favorite_albums = true,
            Self::Is(IsFlag::FavoriteWork) => needs.favorite_works = true,
            Self::Is(IsFlag::Missing) => (),
            Self::Is(IsFlag::Tagged(_)) => needs.fields = true,
            Self::Not(node) => node.add_needs(needs),
            Self::And(nodes) | Self::Or(nodes) => {
                for node in nodes {
                    node.add_needs(needs);
                }
            }
        }
    }

    fn resolve_dates(&mut self, now: i64) {
        match self {
            Self::Term(Term {
                test: Test::Dates(range),
                ..
            }) => *range = range.at(now),
            Self::Not(node) => node.resolve_dates(now),
            Self::And(nodes) | Self::Or(nodes) => {
                for node in nodes {
                    node.resolve_dates(now);
                }
            }
            _ => (),
        }
    }
}

// A field with a value to compare its values with, e.g. `year:<1970`
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Term {
    field: Field,
    op: Op,
    // The value as it was typed, which a rule is written back with
    raw: String,
    test: Test,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    // `:`, which contains for text, and is equal for numbers
    Contains,
    // `=` or `==`
    Equals,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl Op {
    fn text(self) -> &'static str {
        match self {
            Self::Contains => "",
            Self::Equals => "=",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
        }
    }
}

// What a term compares a field's values with
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Test {
    // A value contains the text, which is folded
    Contains(String),
    // A value is the text
    Equals(String),
    // A value is in the range, for numbers and lengths
    Numbers(NumberRange),
    Dates(DateRange),
    Catalogue(CatalogueTest),
    Path(PathTest),
}

// Where a field's values come from
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    // The lines of the track's fields with these codes, see `FieldParts`
    Tags(&'static [FieldCode]),
    // The path that's shown
    Path,
    // When the track was added to the library, how many times it was
    // played, when it was last heard, and when it was marked a favorite,
    // which the `Candidate` has
    Added,
    Plays,
    Played,
    Favorited,
}

// What a field's values are
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Text,
    Number,
    // In whole seconds, written as `3:30` or `5min`
    Duration,
    Date,
    Catalogue,
    Path,
}

// The fields a search can look into; their names stay in English, so that
// searches and rules are the same in every language
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Composer,
    Lyricist,
    Arranger,
    Performer,
    Conductor,
    Ensemble,
    Soloist,
    Role,
    People,
    Work,
    Movement,
    Collection,
    Form,
    Catalogue,
    Genre,
    Grouping,
    Label,
    CatalogueNumber,
    ReleaseType,
    Media,
    Format,
    MusicBrainzId,
    Path,
    Year,
    Disc,
    Track,
    Recorded,
    Original,
    Released,
    Length,
    SampleRate,
    BitDepth,
    Bitrate,
    Channels,
    // Not from the tags, but from the library and the play stats
    Added,
    Plays,
    Played,
    Favorited,
}

impl Field {
    pub(crate) const ALL: [Self; 42] = [
        Self::Title,
        Self::Artist,
        Self::AlbumArtist,
        Self::Album,
        Self::Composer,
        Self::Lyricist,
        Self::Arranger,
        Self::Performer,
        Self::Conductor,
        Self::Ensemble,
        Self::Soloist,
        Self::Role,
        Self::People,
        Self::Work,
        Self::Movement,
        Self::Collection,
        Self::Form,
        Self::Catalogue,
        Self::Genre,
        Self::Grouping,
        Self::Label,
        Self::CatalogueNumber,
        Self::ReleaseType,
        Self::Media,
        Self::Format,
        Self::MusicBrainzId,
        Self::Path,
        Self::Year,
        Self::Disc,
        Self::Track,
        Self::Recorded,
        Self::Original,
        Self::Released,
        Self::Length,
        Self::SampleRate,
        Self::BitDepth,
        Self::Bitrate,
        Self::Channels,
        Self::Added,
        Self::Plays,
        Self::Played,
        Self::Favorited,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::AlbumArtist => "albumartist",
            Self::Album => "album",
            Self::Composer => "composer",
            Self::Lyricist => "lyricist",
            Self::Arranger => "arranger",
            Self::Performer => "performer",
            Self::Conductor => "conductor",
            Self::Ensemble => "ensemble",
            Self::Soloist => "soloist",
            Self::Role => "role",
            Self::People => "people",
            Self::Work => "work",
            Self::Movement => "movement",
            Self::Collection => "collection",
            Self::Form => "form",
            Self::Catalogue => "catalogue",
            Self::Genre => "genre",
            Self::Grouping => "grouping",
            Self::Label => "label",
            Self::CatalogueNumber => "catno",
            Self::ReleaseType => "releasetype",
            Self::Media => "media",
            Self::Format => "format",
            Self::MusicBrainzId => "mbid",
            Self::Path => "path",
            Self::Year => "year",
            Self::Disc => "disc",
            Self::Track => "track",
            Self::Recorded => "recorded",
            Self::Original => "original",
            Self::Released => "released",
            Self::Length => "length",
            Self::SampleRate => "samplerate",
            Self::BitDepth => "bitdepth",
            Self::Bitrate => "bitrate",
            Self::Channels => "channels",
            Self::Added => "added",
            Self::Plays => "plays",
            Self::Played => "played",
            Self::Favorited => "favorited",
        }
    }

    // A field by its name, ignoring ASCII case
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|field| field.name().eq_ignore_ascii_case(name))
    }

    pub(crate) fn kind(self) -> Kind {
        match self {
            Self::Catalogue => Kind::Catalogue,
            Self::Path => Kind::Path,
            Self::Year
            | Self::Disc
            | Self::Track
            | Self::SampleRate
            | Self::BitDepth
            | Self::Bitrate
            | Self::Channels
            | Self::Plays => Kind::Number,
            Self::Length => Kind::Duration,
            Self::Recorded
            | Self::Original
            | Self::Released
            | Self::Added
            | Self::Played
            | Self::Favorited => Kind::Date,
            _ => Kind::Text,
        }
    }

    // Where its values come from: the lines of the fields, or what the
    // `Candidate` has that isn't in the tags
    pub(crate) fn source(self) -> Source {
        use FieldCode as C;
        let codes: &'static [FieldCode] = match self {
            Self::Title => &[C::Title],
            Self::Artist => &[C::Artist],
            Self::AlbumArtist => &[C::AlbumArtist],
            Self::Album => &[C::Album],
            Self::Composer => &[C::Composer],
            Self::Lyricist => &[C::Lyricist],
            Self::Arranger => &[C::Arranger],
            Self::Performer => &[C::Performer],
            Self::Conductor => &[C::Conductor],
            Self::Ensemble => &[C::Ensemble],
            Self::Soloist => &[C::Soloist],
            Self::Role => &[C::Role],
            Self::People => &[
                C::Artist,
                C::AlbumArtist,
                C::Composer,
                C::Lyricist,
                C::Arranger,
                C::Performer,
            ],
            Self::Work => &[C::Work],
            Self::Movement => &[C::Movement],
            Self::Collection => &[C::Collection],
            Self::Form => &[C::Form],
            Self::Catalogue => &[C::Catalogue],
            Self::Genre => &[C::Genre],
            Self::Grouping => &[C::Grouping],
            Self::Label => &[C::Label],
            Self::CatalogueNumber => &[C::CatalogueNumber],
            Self::ReleaseType => &[C::ReleaseType],
            Self::Media => &[C::Media],
            Self::Format => &[C::Format],
            Self::MusicBrainzId => &[C::MusicBrainzId],
            Self::Path => return Source::Path,
            Self::Year => &[C::Year],
            Self::Disc => &[C::Disc],
            Self::Track => &[C::Track],
            Self::Recorded => &[C::Recorded],
            Self::Original => &[C::Original],
            Self::Released => &[C::Released],
            Self::Length => &[C::Length],
            Self::SampleRate => &[C::SampleRate],
            Self::BitDepth => &[C::BitDepth],
            Self::Bitrate => &[C::Bitrate],
            Self::Channels => &[C::Channels],
            Self::Added => return Source::Added,
            Self::Plays => return Source::Plays,
            Self::Played => return Source::Played,
            Self::Favorited => return Source::Favorited,
        };
        Source::Tags(codes)
    }

    // A search with a field of numbers, for the problem of one that isn't
    // a number; the year is when a track was recorded or released, so it's
    // never one of the 18th century
    fn example(self) -> &'static str {
        match self {
            Self::Year => "year:1965 or year:1960..1969",
            Self::Disc => "disc:2",
            Self::Track => "track:1..3",
            Self::SampleRate => "samplerate:>=96000",
            Self::BitDepth => "bitdepth:24",
            Self::Bitrate => "bitrate:>=256",
            Self::Channels => "channels:>2",
            Self::Plays => "plays:>=10",
            _ => "",
        }
    }

    fn add_needs(self, needs: &mut Needs) {
        match self.source() {
            Source::Tags(_) => needs.fields = true,
            Source::Path => needs.path = true,
            Source::Added => needs.added = true,
            Source::Plays | Source::Played => needs.stats = true,
            Source::Favorited => needs.favorites = true,
        }
    }

    // What the field looks into, for the help on the syntax
    pub(crate) fn description(self) -> String {
        match self {
            Self::Title => i18n("The title"),
            Self::Artist => i18n("The artist"),
            Self::AlbumArtist => i18n("The album artist"),
            Self::Album => i18n("The album, or its folder"),
            Self::Composer => i18n("The composers"),
            Self::Lyricist => i18n("The lyricists"),
            Self::Arranger => i18n("The arrangers"),
            Self::Performer => i18n("The performers, with their roles, and the conductors"),
            Self::Conductor => i18n("The conductors"),
            Self::Ensemble => i18n("The orchestras, choirs and other groups"),
            Self::Soloist => i18n("The soloists and other players"),
            Self::Role => i18n("What the performers play or sing, e.g. piano"),
            Self::People => i18n("Anyone credited, but not the titles"),
            Self::Work => i18n("The work, at every level, with its other names"),
            Self::Movement => i18n("The movement"),
            Self::Collection => i18n("The collection the work is in"),
            Self::Form => i18n("The kind of work, e.g. cantata or symphony"),
            Self::Catalogue => {
                i18n("A catalogue number of the work, e.g. bwv1047, or a range: bwv1046..1051")
            }
            Self::Genre => i18n("The genre"),
            Self::Grouping => i18n("The grouping"),
            Self::Label => i18n("The record label"),
            Self::CatalogueNumber => i18n("The catalogue number of the release"),
            Self::ReleaseType => i18n("The kind of release, e.g. live or compilation"),
            Self::Media => i18n("The medium, e.g. CD or vinyl"),
            Self::Format => i18n("The file format, e.g. FLAC"),
            Self::MusicBrainzId => i18n("A MusicBrainz id"),
            Self::Path => {
                i18n("The folders and the file, from the start of a name, or of the whole path")
            }
            Self::Year => i18n("The year it was recorded, or else first released"),
            Self::Disc => i18n("The disc number"),
            Self::Track => i18n("The track number"),
            Self::Recorded => i18n("When it was recorded"),
            Self::Original => i18n("When it was first released"),
            Self::Released => i18n("When this release came out"),
            Self::Length => i18n("How long it is, e.g. >10min or 3:00..5:00"),
            Self::SampleRate => i18n("The sample rate, in Hz"),
            Self::BitDepth => i18n("The bit depth"),
            Self::Bitrate => i18n("The bitrate, in kbps"),
            Self::Channels => i18n("The number of channels"),
            Self::Added => i18n("When it was added to the library"),
            Self::Plays => i18n("How many times it was played, all time"),
            Self::Played => i18n("When it was last listened to, for 30 seconds or more"),
            Self::Favorited => i18n("When it was marked with the heart"),
        }
    }
}

// What `is:` can test
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum IsFlag {
    Favorite,
    // In a favorite album
    FavoriteAlbum,
    // A part of a favorite work, or of a work with a favorite recording
    FavoriteWork,
    // A flag the fields keep, see `FieldCode::Is`
    Tagged(Flag),
    // A track whose file is gone, in the queue
    Missing,
}

impl IsFlag {
    pub(crate) const ALL: [Self; 9] = [
        Self::Favorite,
        Self::FavoriteAlbum,
        Self::FavoriteWork,
        Self::Tagged(Flag::Movement),
        Self::Tagged(Flag::Arrangement),
        Self::Tagged(Flag::Live),
        Self::Tagged(Flag::Partial),
        Self::Tagged(Flag::Lossless),
        Self::Missing,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Favorite => "favorite",
            Self::FavoriteAlbum => "favorite-album",
            Self::FavoriteWork => "favorite-work",
            Self::Tagged(flag) => flag.name(),
            Self::Missing => "missing",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|flag| flag.name().eq_ignore_ascii_case(name))
    }

    pub(crate) fn description(self) -> String {
        match self {
            Self::Favorite => i18n("Tracks marked with the heart"),
            Self::FavoriteAlbum => i18n("Tracks of albums marked with the heart"),
            Self::FavoriteWork => {
                i18n("Tracks of works marked with the heart, or of their marked recordings")
            }
            Self::Tagged(Flag::Movement) => i18n("Movements of a larger work"),
            Self::Tagged(Flag::Arrangement) => i18n("Arrangements of another work"),
            Self::Tagged(Flag::Live) => i18n("Live recordings"),
            Self::Tagged(Flag::Partial) => i18n("Recordings of part of a work"),
            Self::Tagged(Flag::Lossless) => i18n("Files in a lossless format"),
            Self::Missing => i18n("Tracks whose files are gone"),
        }
    }
}

// How smart playlists will choose and order their tracks, written in the
// rule; a search ignores them
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Directives {
    pub(crate) sort: Vec<SortKey>,
    // The order to choose tracks in for the limit, if it isn't `sort`
    pub(crate) pick: Vec<SortKey>,
    pub(crate) limit: Option<Limit>,
    pub(crate) by: Option<Unit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SortKey {
    pub(crate) by: SortBy,
    pub(crate) descending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SortBy {
    // The queue's sorts, see `audio::SortKey`
    Album,
    Artist,
    Composer,
    Title,
    Year,
    FileName,
    Random,
    // The queue's shuffle, by the play stats
    Shuffle,
    // A field with numbers, lengths or dates
    Field(Field),
}

impl SortBy {
    fn name(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Composer => "composer",
            Self::Title => "title",
            Self::Year => "year",
            Self::FileName => "file-name",
            Self::Random => "random",
            Self::Shuffle => "shuffle",
            Self::Field(field) => field.name(),
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [
            Self::Album,
            Self::Artist,
            Self::Composer,
            Self::Title,
            Self::Year,
            Self::FileName,
            Self::Random,
            Self::Shuffle,
        ]
        .into_iter()
        .find(|by| by.name().eq_ignore_ascii_case(name))
        .or_else(|| {
            Field::from_name(name)
                .filter(|field| matches!(field.kind(), Kind::Number | Kind::Duration | Kind::Date))
                .map(Self::Field)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    // Units, see `Unit`
    Count(u32),
    // In seconds
    Duration(u64),
}

// What `pick`, `limit` and `sort` move as one
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    // A recording's movements of a work, kept together and in order, and
    // each other track on its own
    Work,
    // The tracks of an album
    Album,
    Track,
}

impl Unit {
    fn name(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Album => "album",
            Self::Track => "track",
        }
    }
}

// The directives, by name
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Directive {
    Sort,
    Pick,
    Limit,
    By,
}

impl Directive {
    const ALL: [Self; 4] = [Self::Sort, Self::Pick, Self::Limit, Self::By];

    fn name(self) -> &'static str {
        match self {
            Self::Sort => "sort",
            Self::Pick => "pick",
            Self::Limit => "limit",
            Self::By => "by",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|directive| directive.name().eq_ignore_ascii_case(name))
    }
}

// Something wrong with a search, or worth knowing, at a place in its text
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    // In bytes of the text
    pub(crate) span: Range<usize>,
    pub(crate) kind: ProblemKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProblemKind {
    // Errors in a search and in a rule
    NotANumber(Field),
    NotADate(Field),
    NotALength(Field),
    NotACatalogueNumber,
    CantCompare(Field),
    ReversedRange,
    UnknownFlag(String),
    UnknownField(String),
    FieldInGroup,
    // `field::`, for regular expressions later
    Reserved,
    // Hints in a search, errors in a rule
    LoneBang,
    NoFieldToCompare,
    DidYouMean { typed: String, field: &'static str },
    TooDeep,
    // Only in a search
    DirectiveInSearch,
    // Only in a rule
    NotAField(String),
    Incomplete,
    ReservedCharacter(char),
    EqualsEmpty(Field),
    BadDirective(&'static str),
}

impl Problem {
    // Whether the query can't be used: a search with an error matches
    // nothing, and a rule with one can't be saved
    pub(crate) fn is_error(&self, mode: Mode) -> bool {
        use ProblemKind as K;
        match &self.kind {
            K::NotANumber(_)
            | K::NotADate(_)
            | K::NotALength(_)
            | K::NotACatalogueNumber
            | K::CantCompare(_)
            | K::ReversedRange
            | K::UnknownFlag(_)
            | K::UnknownField(_)
            | K::FieldInGroup
            | K::Reserved => true,
            K::LoneBang | K::NoFieldToCompare | K::DidYouMean { .. } | K::TooDeep => {
                mode == Mode::Rule
            }
            K::DirectiveInSearch => false,
            K::NotAField(_)
            | K::Incomplete
            | K::ReservedCharacter(_)
            | K::EqualsEmpty(_)
            | K::BadDirective(_) => true,
        }
    }

    // What to tell the user. The names of fields and the syntax stay in
    // English, as they're typed
    pub(crate) fn message(&self) -> String {
        use ProblemKind as K;
        let field_name = |field: &Field| format!("{}:", field.name());
        match &self.kind {
            // Translators: `{field}` and `{example}` must be left unmodified;
            // they're expanded to the name of a field of the search, e.g.
            // "year:", and to a search with it, e.g. "year:1960..1969"
            K::NotANumber(field) => i18n_k(
                "{field} takes a number, e.g. {example}",
                &[("field", &field_name(field)), ("example", field.example())],
            ),
            // Translators: `{field}` must be left unmodified; it's expanded
            // to the name of a field of the search, e.g. "released:". The
            // examples are the syntax, and stay as they are
            K::NotADate(field) => i18n_k(
                "{field} takes a date, e.g. {field}2026-10 or {field}-30d for the last 30 days",
                &[("field", &field_name(field))],
            ),
            // Translators: `{field}` must be left unmodified; it's expanded
            // to the name of a field of the search, e.g. "length:"
            K::NotALength(field) => i18n_k(
                "{field} takes a length, e.g. {field}3:30 or {field}>10min",
                &[("field", &field_name(field))],
            ),
            K::NotACatalogueNumber => i18n(
                "catalogue: takes a catalogue number, e.g. catalogue:bwv1047, catalogue:op27 or catalogue:bwv",
            ),
            // Translators: `{field}` must be left unmodified; it's expanded
            // to the name of a field of the search, e.g. "title:"
            K::CantCompare(field) => i18n_k(
                "{field} can't be compared with < or >",
                &[("field", &field_name(field))],
            ),
            K::ReversedRange => i18n("A range goes from the smaller value to the larger one"),
            // Translators: `{value}` must be left unmodified; it's expanded
            // to what was typed after "is:". The names after "is:" stay as
            // they are
            K::UnknownFlag(value) => i18n_k(
                "is:{value} isn't known; is: takes favorite, favorite-album, favorite-work, movement, arrangement, live, partial, lossless or missing",
                &[("value", value)],
            ),
            // Translators: `{value}` must be left unmodified; it's expanded
            // to what was typed after "has:"
            K::UnknownField(value) => i18n_k(
                "has:{value} isn't known; has: takes the name of a field, e.g. has:composer",
                &[("value", value)],
            ),
            K::FieldInGroup => i18n("A field can't go inside the parentheses of another field"),
            K::Reserved => i18n("“::” is kept for regular expressions, which aren't supported yet"),
            K::LoneBang => i18n("To leave tracks out, put “-” before a word, e.g. -live"),
            K::NoFieldToCompare => {
                i18n("Comparing needs a field, e.g. year:>2000; this is searched as text")
            }
            // Translators: `{typed}` and `{field}` must be left unmodified;
            // they're expanded to what was typed, e.g. "composr:", and to the
            // name of a field of the search, e.g. "composer:"
            K::DidYouMean { typed, field } => i18n_k(
                "{typed}: isn't a field, and is searched as text; did you mean {field}:?",
                &[("typed", typed), ("field", field)],
            ),
            K::TooDeep => {
                i18n("Too many parentheses inside each other; the inner ones are ignored")
            }
            K::DirectiveInSearch => i18n("sort:, pick:, limit: and by: don't apply to searches"),
            // Translators: `{name}` must be left unmodified; it's expanded
            // to what was typed before a colon, e.g. "composr"
            K::NotAField(name) => i18n_k("{name}: isn't a field", &[("name", name)]),
            K::Incomplete => i18n("This isn't finished"),
            // Translators: `{character}` must be left unmodified; it's
            // expanded to a character such as "*"
            K::ReservedCharacter(character) => i18n_k(
                "Put a value with “{character}” in quotes",
                &[("character", &character.to_string())],
            ),
            // Translators: `{field}` must be left unmodified; it's expanded
            // to the name of a field of the search, e.g. "composer"
            K::EqualsEmpty(field) => i18n_k(
                "To find tracks without a value, use -has:{field}",
                &[("field", field.name())],
            ),
            K::BadDirective(directive) => match *directive {
                "sort" | "pick" => i18n(
                    "sort: and pick: take a field with numbers or dates, album, artist, composer, title, file-name, random or shuffle",
                ),
                "limit" => i18n("limit: takes a number, or a length, e.g. limit:100 or limit:2h"),
                _ => i18n("by: takes work, album or track"),
            },
        }
    }
}

#[cfg(test)]
mod tests;
