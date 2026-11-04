// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The values of a track that the fields of the search syntax look into,
//! e.g. `composer:bach` or `year:<1970`. They're packed into one string of
//! lines, `code\tvalue\n`, each value folded as searches are
//! (`normalize_search_text()`). The library keeps the string in its
//! `fields` column, and each track of the queue keeps its own copy. Both
//! are made here, from the same parts, so that the two searches find the
//! same tracks.

use std::collections::{BTreeSet, HashSet};

use super::{
    search::{MAX_SEARCHED_TEXT, text_start},
    *,
};
use crate::catalogue;

// The longest a track can be. A file can report a far longer one in its
// header, e.g. an Ogg file cut off in the middle of a page, and the clock
// times of GStreamer and MPRIS can't hold the longest; such a length counts
// as unknown. Audiobooks in a single file run to some tens of hours
const MAX_DURATION: Duration = Duration::from_secs(1000 * 60 * 60);

// The length of a track, zero if it's unknown
pub(super) fn sane_length(length: Duration) -> Duration {
    if length > MAX_DURATION {
        warn!("Ignoring a length of {} s", length.as_secs());
        Duration::ZERO
    } else {
        length
    }
}

// The length of a track in seconds, 0 if it's unknown
fn sane_duration(duration: Duration) -> u64 {
    sane_length(duration).as_secs()
}

// What each line of the fields holds. The codes are kept in the library,
// so changing one needs a new version of its schema
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FieldCode {
    Title,
    Artist,
    AlbumArtist,
    Album,
    // The names and the sort names
    Composer,
    Lyricist,
    Arranger,
    // Every performer credited, as "name (role)" when there's a role, and
    // the conductors
    Performer,
    // The performers of each kind, as the Performers list sorts them
    Conductor,
    Ensemble,
    Soloist,
    // The roles of the performers, e.g. "piano"
    Role,
    // The names of the work, at every level, its other names and its
    // collections
    Work,
    Movement,
    Collection,
    // The MusicBrainz types of the work and of its parts, e.g. "cantata"
    Form,
    // A catalogue number of the work, e.g. "bwv 1047", see
    // `CatalogueNumber::search_text()`
    Catalogue,
    Genre,
    Grouping,
    Label,
    // The release's catalogue number
    CatalogueNumber,
    ReleaseType,
    Media,
    Format,
    MusicBrainzId,
    Year,
    Disc,
    Track,
    // The dates of the recording, of the first release and of this one,
    // as the tags give them
    Recorded,
    Original,
    Released,
    // A flag, see `Flag`
    Is,
    // In whole seconds
    Length,
    SampleRate,
    BitDepth,
    // In kbps
    Bitrate,
    Channels,
}

impl FieldCode {
    // The search syntax will need these to read the lines back
    #[cfg(test)]
    const ALL: [Self; 37] = [
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
        Self::Year,
        Self::Disc,
        Self::Track,
        Self::Recorded,
        Self::Original,
        Self::Released,
        Self::Is,
        Self::Length,
        Self::SampleRate,
        Self::BitDepth,
        Self::Bitrate,
        Self::Channels,
    ];

    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Title => "ti",
            Self::Artist => "ar",
            Self::AlbumArtist => "aa",
            Self::Album => "al",
            Self::Composer => "co",
            Self::Lyricist => "ly",
            Self::Arranger => "an",
            Self::Performer => "pf",
            Self::Conductor => "cd",
            Self::Ensemble => "en",
            Self::Soloist => "so",
            Self::Role => "ro",
            Self::Work => "wk",
            Self::Movement => "mv",
            Self::Collection => "cl",
            Self::Form => "fm",
            Self::Catalogue => "ca",
            Self::Genre => "ge",
            Self::Grouping => "gr",
            Self::Label => "lb",
            Self::CatalogueNumber => "cn",
            Self::ReleaseType => "rt",
            Self::Media => "md",
            Self::Format => "ft",
            Self::MusicBrainzId => "id",
            Self::Year => "yr",
            Self::Disc => "dn",
            Self::Track => "tn",
            Self::Recorded => "rd",
            Self::Original => "od",
            Self::Released => "re",
            Self::Is => "is",
            Self::Length => "ln",
            Self::SampleRate => "sr",
            Self::BitDepth => "bd",
            Self::Bitrate => "br",
            Self::Channels => "ch",
        }
    }

    #[cfg(test)]
    pub(crate) fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.code() == code)
    }
}

// The flags of a track, the values of its `FieldCode::Is` lines
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Flag {
    // Part of a larger work
    Movement,
    // An arrangement of another work
    Arrangement,
    // A live performance, or on a live album
    Live,
    // A recording of part of a work, or of a movement
    Partial,
    // In a format that loses nothing, as its bit depth says
    Lossless,
}

impl Flag {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Movement => "movement",
            Self::Arrangement => "arrangement",
            Self::Live => "live",
            Self::Partial => "partial",
            Self::Lossless => "lossless",
        }
    }
}

// What a track's file says about its sound, for the fields
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AudioProperties {
    // In whole seconds, 0 if it's unknown
    pub(crate) length: u64,
    pub(crate) sample_rate: Option<u32>,
    pub(crate) bit_depth: Option<u8>,
    // In kbps
    pub(crate) bitrate: Option<u32>,
    pub(crate) channels: Option<u8>,
}

impl AudioProperties {
    // As `FileDetails` takes them, without the values that mean unknown
    pub(crate) fn of(properties: &lofty::properties::FileProperties) -> Self {
        Self {
            length: sane_duration(properties.duration()),
            sample_rate: properties.sample_rate().filter(|r| *r > 0),
            bit_depth: properties.bit_depth().filter(|d| *d > 0),
            bitrate: properties.audio_bitrate().filter(|b| *b > 0),
            channels: properties.channels().filter(|c| *c > 0),
        }
    }

    // For a part of the file, as a track of a CUE sheet plays
    pub(crate) fn with_length(self, length: Duration) -> Self {
        Self {
            length: sane_duration(length),
            ..self
        }
    }
}

// What the fields of a track are made from
pub(crate) struct FieldParts<'a> {
    pub(crate) tag: &'a Tag,
    pub(crate) credits: &'a TrackCredits,
    // The conductors, performers and artists the list of performers has,
    // see `TrackCredits::listed_performers()` and `listed_artists()`
    pub(crate) performers: &'a [ListedPerformer],
    // The title that searches look into, see `TrackData::searched_title()`
    pub(crate) title: Option<&'a str>,
    // The album, or else the name of its folder, see `folder_album_title()`
    pub(crate) album: Option<&'a str>,
    pub(crate) format: Option<&'a str>,
    // `None` if the file's properties weren't read
    pub(crate) properties: Option<AudioProperties>,
}

impl FieldParts<'_> {
    // The lines of the fields
    pub(crate) fn encode(&self) -> String {
        let mut lines = Lines::default();
        let credits = self.credits;
        let tag = self.tag;

        lines.push(FieldCode::Title, self.title);
        lines.extend(FieldCode::Artist, tag.get_strings(ItemKey::TrackArtist));
        lines.extend(
            FieldCode::AlbumArtist,
            tag.get_strings(ItemKey::AlbumArtist),
        );
        lines.push(FieldCode::Album, self.album);

        lines.extend(FieldCode::Composer, &credits.composers);
        lines.extend(FieldCode::Composer, &credits.composer_sorts);
        lines.extend(FieldCode::Lyricist, &credits.lyricists);
        lines.extend(FieldCode::Arranger, &credits.arrangers);

        for credit in &credits.performers {
            let credited = match &credit.role {
                Some(role) => format!("{} ({role})", credit.name),
                None => credit.name.clone(),
            };
            lines.push_string(FieldCode::Performer, &credited);
            lines.push(FieldCode::Role, credit.role.as_deref());
        }
        lines.extend(FieldCode::Performer, &credits.conductors);
        for performer in self.performers {
            lines.push(FieldCode::Performer, Some(&performer.name));
            let kind = match performer.kind {
                PerformerKind::Conductor => FieldCode::Conductor,
                PerformerKind::Ensemble => FieldCode::Ensemble,
                PerformerKind::Soloist => FieldCode::Soloist,
                // A performer, as the list of performers has it, and also
                // found as the artist and the album artist
                PerformerKind::Artist => continue,
            };
            lines.push(kind, Some(&performer.name));
            lines.push(FieldCode::Role, performer.role.as_deref());
        }

        lines.push(FieldCode::Work, credits.work.as_deref());
        lines.push(FieldCode::Work, credits.work_title.as_deref());
        lines.extend(FieldCode::Work, &credits.work_names);
        lines.push(FieldCode::Movement, credits.movement.as_deref());
        lines.push(FieldCode::Collection, credits.work_collection.as_deref());
        lines.extend(FieldCode::Form, &credits.work_forms);
        for number in catalogue_numbers(credits, self.title) {
            lines.push_string(FieldCode::Catalogue, &number.search_text());
        }

        lines.extend(FieldCode::Genre, &credits.genres);
        lines.extend(FieldCode::Grouping, tag.get_strings(ItemKey::ContentGroup));
        lines.extend(FieldCode::Label, &credits.labels);
        lines.extend(FieldCode::CatalogueNumber, &credits.catalog_numbers);
        lines.extend(FieldCode::ReleaseType, &credits.release_types);
        lines.extend(FieldCode::Media, &credits.media);
        lines.push(FieldCode::Format, self.format);

        let ids = &credits.musicbrainz;
        for id in [&ids.recording, &ids.track, &ids.release, &ids.release_group]
            .into_iter()
            .chain([&ids.work, &credits.work_id])
        {
            lines.push(FieldCode::MusicBrainzId, id.as_deref());
        }
        lines.extend(FieldCode::MusicBrainzId, &ids.artists);
        lines.extend(FieldCode::MusicBrainzId, &ids.release_artists);
        lines.extend(FieldCode::MusicBrainzId, &credits.work_part_ids);
        lines.extend(
            FieldCode::MusicBrainzId,
            credits.composer_ids.iter().flatten(),
        );

        lines.push(FieldCode::Year, credits.year().as_deref());
        lines.push_number(FieldCode::Disc, tag.disk());
        lines.push_number(FieldCode::Track, tag.track());
        lines.push(FieldCode::Recorded, credits.recording_date.as_deref());
        lines.push(FieldCode::Original, credits.original_date.as_deref());
        let released = credits.release_date.as_ref().or(credits.date.as_ref());
        lines.push(FieldCode::Released, released.map(String::as_str));

        let is_live = credits.performance_attributes.iter().any(|a| a == "live")
            || credits
                .release_types
                .iter()
                .any(|kind| kind.eq_ignore_ascii_case("live"));
        let flags = [
            (Flag::Movement, credits.is_movement()),
            (Flag::Arrangement, credits.work_arrangement.is_some()),
            (Flag::Live, is_live),
            (
                Flag::Partial,
                credits
                    .performance_attributes
                    .iter()
                    .any(|a| a == "partial"),
            ),
            (
                Flag::Lossless,
                self.properties.is_some_and(|p| p.bit_depth.is_some()),
            ),
        ];
        for (flag, set) in flags {
            if set {
                lines.push(FieldCode::Is, Some(flag.name()));
            }
        }

        if let Some(properties) = self.properties {
            lines.push_number(
                FieldCode::Length,
                Some(properties.length).filter(|l| *l > 0),
            );
            lines.push_number(FieldCode::SampleRate, properties.sample_rate);
            lines.push_number(FieldCode::BitDepth, properties.bit_depth);
            lines.push_number(FieldCode::Bitrate, properties.bitrate);
            lines.push_number(FieldCode::Channels, properties.channels);
        }

        lines.text
    }
}

// The values of the fields `codes` in a track's fields, as `encode()`
// writes them
pub(crate) fn field_values<'a>(
    fields: &'a str,
    codes: &'a [FieldCode],
) -> impl Iterator<Item = &'a str> + 'a {
    fields.lines().filter_map(move |line| {
        let (code, value) = line.split_once('\t')?;
        codes
            .iter()
            .any(|field| field.code() == code)
            .then_some(value)
    })
}

// The catalogue numbers of a track: those of its work, or of its title,
// which names the work when it has no work tag, e.g. "Partita no. 2 in D
// minor, BWV 1004: V. Chaconne"
pub(super) fn catalogue_numbers(
    credits: &TrackCredits,
    title: Option<&str>,
) -> Vec<catalogue::CatalogueNumber> {
    let names = [
        credits.work.as_deref(),
        credits.work_title.as_deref(),
        title,
    ];
    let numbers = names
        .into_iter()
        .flatten()
        .flat_map(catalogue::catalogue_numbers)
        .collect();
    first_occurrences(numbers)
}

// The numbers, each at its first place only. The ones seen are kept in a
// set, rather than each compared with all the ones before it: a crafted
// name can still give about a thousand numbers in the part of it that's
// read (`catalogue::catalogue_numbers`)
fn first_occurrences(numbers: Vec<catalogue::CatalogueNumber>) -> Vec<catalogue::CatalogueNumber> {
    let first: Vec<bool> = {
        let mut seen = BTreeSet::new();
        numbers.iter().map(|number| seen.insert(number)).collect()
    };
    numbers
        .into_iter()
        .zip(first)
        .filter_map(|(number, first)| first.then_some(number))
        .collect()
}

// The lines, each value folded and written once per field
#[derive(Default)]
struct Lines {
    text: String,
    // The lines written, as a track that credits every player of an
    // orchestra has hundreds
    written: HashSet<String>,
}

impl Lines {
    fn push(&mut self, field: FieldCode, value: Option<&str>) {
        if let Some(value) = value {
            self.push_string(field, value);
        }
    }

    fn push_string(&mut self, field: FieldCode, value: &str) {
        // Only the start of a long value, as for the search text: the
        // library reads the fields of every track for each search with a
        // field, and the queue keeps a copy for each of its entries, so a
        // crafted tag of megabytes would be kept and read whole
        let value = normalize_search_text(text_start(value, MAX_SEARCHED_TEXT));
        if value.is_empty() {
            return;
        }
        let line = format!("{}\t{value}\n", field.code());
        if !self.written.contains(&line) {
            self.text.push_str(&line);
            self.written.insert(line);
        }
    }

    fn push_number<T: std::fmt::Display>(&mut self, field: FieldCode, value: Option<T>) {
        if let Some(value) = value {
            self.push_string(field, &value.to_string());
        }
    }

    fn extend<S: AsRef<str>>(&mut self, field: FieldCode, values: impl IntoIterator<Item = S>) {
        for value in values {
            self.push_string(field, value.as_ref());
        }
    }
}

#[cfg(test)]
mod tests {
    use lofty::tag::TagType;

    use super::*;

    #[test]
    fn codes_are_told_apart() {
        for field in FieldCode::ALL {
            assert_eq!(FieldCode::from_code(field.code()), Some(field));
        }
        assert_eq!(FieldCode::ALL.iter().map(|f| f.code()).unique().count(), 37);
    }

    #[test]
    fn the_fields_keep_the_start_of_long_values() {
        let mut tag = Tag::new(TagType::VorbisComments);
        // Two-byte characters, so that the limit falls inside one
        tag.insert_text(ItemKey::TrackTitle, "é".repeat(1_000_000));
        let credits = TrackCredits::from_tag(&tag, &ExtraComments::default());
        let fields = FieldParts {
            tag: &tag,
            credits: &credits,
            performers: &credits.listed_performers(),
            title: tag.title().as_deref(),
            album: None,
            format: None,
            properties: None,
        }
        .encode();
        let title = fields
            .lines()
            .find_map(|line| line.strip_prefix("ti\t"))
            .unwrap();
        assert_eq!(title, "e".repeat(MAX_SEARCHED_TEXT / 2));
        assert!(fields.len() < 2 * MAX_SEARCHED_TEXT);
    }

    #[test]
    fn the_fields_have_a_line_for_each_value() {
        let mut tag = Tag::new(TagType::VorbisComments);
        for (key, value) in [
            (
                ItemKey::TrackTitle,
                "Brandenburg Concerto No. 2 in F major, BWV 1047: I. Allegro",
            ),
            (ItemKey::TrackArtist, "Musica Antiqua Köln"),
            (ItemKey::TrackArtist, "Reinhard  Goebel"),
            (ItemKey::AlbumArtist, "Musica Antiqua Köln, Reinhard Goebel"),
            (ItemKey::Composer, "Johann Sebastian Bach"),
            (ItemKey::Performer, "Reinhard Goebel (violin)"),
            (ItemKey::Conductor, "Reinhard Goebel"),
            (ItemKey::Genre, "Baroque"),
            (ItemKey::ContentGroup, "Concertos"),
            (ItemKey::DiscNumber, "1"),
            (ItemKey::TrackNumber, "7"),
            (ItemKey::RecordingDate, "1986-11"),
        ] {
            tag.push_unchecked(TagItem::new(key, ItemValue::Text(value.to_string())));
        }
        let credits = TrackCredits::from_tag(&tag, &ExtraComments::default());
        let fields = FieldParts {
            tag: &tag,
            credits: &credits,
            performers: &credits.listed_with_artists(None),
            title: tag.title().as_deref(),
            album: Some("Brandenburgische Konzerte"),
            format: Some("FLAC"),
            properties: Some(AudioProperties {
                length: 301,
                sample_rate: Some(96_000),
                bit_depth: Some(24),
                bitrate: Some(2_300),
                channels: Some(2),
            }),
        }
        .encode();

        for line in [
            "ar\tmusica antiqua koln",
            // The spaces collapsed, as in a search
            "ar\treinhard goebel",
            "co\tjohann sebastian bach",
            "aa\tmusica antiqua koln, reinhard goebel",
            "pf\treinhard goebel (violin)",
            "pf\treinhard goebel",
            "ro\tviolin",
            "cd\treinhard goebel",
            // The group in the album artist, as the list of performers
            // has it
            "pf\tmusica antiqua koln",
            "al\tbrandenburgische konzerte",
            "ca\tbwv 1047",
            "ge\tbaroque",
            "gr\tconcertos",
            "ft\tflac",
            "dn\t1",
            "tn\t7",
            // DATE, which lofty takes for the recording's
            "yr\t1986",
            "re\t1986-11",
            "is\tlossless",
            "ln\t301",
            "sr\t96000",
            "bd\t24",
            "br\t2300",
            "ch\t2",
        ] {
            assert!(
                fields.contains(&format!("{line}\n")),
                "{line} in {fields:?}"
            );
        }
        // Each value once
        assert_eq!(fields.matches("pf\treinhard goebel\n").count(), 1);
        assert!(!fields.contains("is\tlive"));
    }

    #[test]
    fn a_track_whose_properties_werent_read_has_no_lines_for_them() {
        let tag = Tag::new(TagType::VorbisComments);
        let credits = TrackCredits::from_tag(&tag, &ExtraComments::default());
        let fields = FieldParts {
            tag: &tag,
            credits: &credits,
            performers: &credits.listed_performers(),
            title: Some("Song"),
            album: None,
            format: None,
            properties: None,
        }
        .encode();
        assert_eq!(fields, "ti\tsong\n");
    }

    fn search_texts(numbers: &[catalogue::CatalogueNumber]) -> Vec<String> {
        numbers
            .iter()
            .map(catalogue::CatalogueNumber::search_text)
            .collect()
    }

    #[test]
    fn a_catalogue_number_named_again_keeps_its_first_place() {
        let credits = TrackCredits {
            work: Some("Sonatas, K. 380, K. 141".to_string()),
            work_title: Some("Sonatas: K. 141, K. 9, K. 141".to_string()),
            ..TrackCredits::default()
        };
        let numbers = catalogue_numbers(&credits, Some("K. 9, K. 380, K. 27"));
        assert_eq!(search_texts(&numbers), ["k 380", "k 141", "k 9", "k 27"]);
    }

    #[test]
    fn a_title_that_names_many_catalogue_numbers_keeps_them_all_in_order() {
        // As many as fit in the 4 KiB of a name that are read
        let title = (1..=800).map(|n| format!("d{n}")).join(" ");
        assert!(title.len() < 4096);
        let credits = TrackCredits {
            work: Some("D. 800, D. 7".to_string()),
            ..TrackCredits::default()
        };
        let numbers = catalogue_numbers(&credits, Some(&title));
        let expected: Vec<String> = [800, 7]
            .into_iter()
            .chain((1..800).filter(|n| *n != 7))
            .map(|n| format!("d {n}"))
            .collect();
        assert_eq!(search_texts(&numbers), expected);
    }

    #[test]
    fn catalogue_numbers_are_kept_once_as_comparing_each_with_those_before_did() {
        let names = [
            (0..1_000).map(|n| format!("BWV {}", n % 300)).join(", "),
            (0..1_000)
                .map(|n| format!("op. {} no. {}", n % 40, n % 7))
                .join(", "),
            (0..1_000)
                .map(|n| format!("BWV {}, op. {}", n % 500, n % 60))
                .join("; "),
        ];
        let credits = TrackCredits {
            work: Some(names[0].clone()),
            work_title: Some(names[1].clone()),
            ..TrackCredits::default()
        };
        let numbers = catalogue_numbers(&credits, Some(&names[2]));

        let mut expected: Vec<catalogue::CatalogueNumber> = Vec::new();
        for number in names
            .iter()
            .flat_map(|name| catalogue::catalogue_numbers(name))
        {
            if !expected.contains(&number) {
                expected.push(number);
            }
        }
        assert_eq!(numbers, expected);
    }
}
