// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    fs::File,
    hash::Hash,
    io::BufReader,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};
use itertools::Itertools;
use lofty::{
    ape::{ApeFile, ApeTag},
    config::ParseOptions,
    error::FileParseError,
    file::{AudioFile, FileType, TaggedFile},
    flac::FlacFile,
    ogg::{OpusFile, SpeexFile, VorbisFile, tag::VorbisComments},
    prelude::{Accessor, TaggedFileExt},
    probe::Probe,
    tag::{ItemKey, ItemValue, Tag},
    wavpack::WavPackFile,
};
use log::{debug, warn};
use sha2::{Digest, Sha256};

use crate::{
    audio::{
        cover_cache::{AlbumCover, CoverArt, CoverCache},
        youtube_description,
    },
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::i18n,
    library::is_mbid,
    utils::ATTRIBUTE_HOST_PATH,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Credit {
    pub name: String,
    pub role: Option<String>,
}

impl Credit {
    // MusicBrainz Picard stores performers as "Name (role)"
    fn parse(s: &str) -> Self {
        if let Some(name) = s.strip_suffix(')')
            && let Some((name, role)) = name.rsplit_once(" (")
        {
            return Credit {
                name: name.trim().to_string(),
                role: Some(role.trim().to_string()),
            };
        }

        Credit {
            name: s.to_string(),
            role: None,
        }
    }
}

// The additional credits and release information in the tags, mostly
// relevant for classical music
#[derive(Debug, Clone, Default)]
pub struct TrackCredits {
    pub work: Option<String>,
    pub movement: Option<String>,
    pub movement_number: Option<String>,
    // The levels of the work above the movement, and the movement, when
    // the movement is part of a part of the work, e.g. "Book I: Prelude
    // and Fugue no. 1 in C major" and "1. Prelude"; the queue shows them
    // on separate lines, as the movement would be cut off
    pub movement_levels: Option<(String, String)>,
    pub composers: Vec<String>,
    pub lyricists: Vec<String>,
    pub arrangers: Vec<String>,
    pub conductors: Vec<String>,
    pub performers: Vec<Credit>,
    pub producers: Vec<String>,
    pub engineers: Vec<String>,
    pub mixers: Vec<String>,
    pub remixers: Vec<String>,
    pub dj_mixers: Vec<String>,
    // The production roles without a field of their own, e.g. "mastering
    // engineer"
    pub other_production: Vec<Credit>,
    pub album_artist: Option<String>,
    pub disc_number: Option<u32>,
    pub disc_total: Option<u32>,
    pub disc_subtitle: Option<String>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub date: Option<String>,
    pub original_date: Option<String>,
    // The MusicBrainz release types, e.g. "album" and "live"
    pub release_types: Vec<String>,
    // The medium of the disc, e.g. "CD" or "Digital Media"
    pub media: Vec<String>,
    // A country code, or one of the MusicBrainz codes like "XW" for the
    // whole world
    pub release_country: Option<String>,
    pub labels: Vec<String>,
    pub catalog_numbers: Vec<String>,
    pub barcodes: Vec<String>,
    pub isrcs: Vec<String>,
    pub genres: Vec<String>,
    // Whether the composers come from the description that YouTube
    // generated, rather than from the tags
    pub composers_from_description: bool,
    pub copyright: Vec<String>,
    pub musicbrainz: MusicBrainzIds,
    // The MusicBrainz ids of the composers of the works the track
    // performs, from the "Work Hierarchy" plugin; `None` if it didn't
    // record them
    pub composer_ids: Option<Vec<String>>,
}

// The ids of a track on MusicBrainz; they are only set if they are valid,
// so they can be put in a URL
#[derive(Debug, Clone, Default)]
pub struct MusicBrainzIds {
    pub recording: Option<String>,
    pub release: Option<String>,
    pub work: Option<String>,
    // The ids of the track artists
    pub artists: Vec<String>,
}

impl MusicBrainzIds {
    fn from_tag(tag: &Tag) -> Self {
        let id = |key| {
            tag.get_string(key)
                .filter(|id| is_mbid(id))
                .map(ToString::to_string)
        };
        MusicBrainzIds {
            recording: id(ItemKey::MusicBrainzRecordingId),
            release: id(ItemKey::MusicBrainzReleaseId),
            work: id(ItemKey::MusicBrainzWorkId),
            artists: tag_strings(tag, ItemKey::MusicBrainzArtistId)
                .into_iter()
                .filter(|id| is_mbid(id))
                .collect(),
        }
    }
}

impl TrackCredits {
    // The credits in the tags, filled in from the description that YouTube
    // generated, if the track has one; lofty leaves the tags it doesn't
    // know out of `tag`, so `extra` has the Vorbis comments we need
    fn from_tag(tag: &Tag, extra: &ExtraComments) -> Self {
        let mut credits = Self::from_tag_items(tag);
        credits.composer_ids = extra
            .work_hierarchy
            .as_deref()
            .and_then(work_hierarchy_composer_ids);
        // The hierarchy has the numbers of every level, so the movement
        // includes them where they belong, and the work is always the
        // top-level one, whichever the plugin was set to tag
        if let Some((work, movement)) = extra
            .work_hierarchy
            .as_deref()
            .and_then(|json| work_hierarchy_labels(json, credits.movement_number.as_deref()))
        {
            credits.work = Some(work);
            credits.movement = Some(movement.full());
            credits.movement_number = None;
            credits.movement_levels = movement.levels.map(|levels| (levels, movement.movement));
        }

        let description = description_texts(tag, extra).find_map(youtube_description::parse);
        if let Some(description) = description {
            credits.fill_in(description);
        }

        credits
    }

    fn from_tag_items(tag: &Tag) -> Self {
        TrackCredits {
            work: tag.get_string(ItemKey::Work).map(ToString::to_string),
            movement: tag.get_string(ItemKey::Movement).map(ToString::to_string),
            movement_number: tag
                .get_string(ItemKey::MovementNumber)
                .map(ToString::to_string),
            movement_levels: None,
            composers: tag_strings(tag, ItemKey::Composer),
            lyricists: tag_strings(tag, ItemKey::Lyricist),
            arrangers: tag_strings(tag, ItemKey::Arranger),
            conductors: tag_strings(tag, ItemKey::Conductor),
            performers: tag_strings(tag, ItemKey::Performer)
                .iter()
                .map(|s| Credit::parse(s))
                .collect(),
            producers: tag_strings(tag, ItemKey::Producer),
            engineers: tag_strings(tag, ItemKey::Engineer),
            mixers: tag_strings(tag, ItemKey::MixEngineer),
            remixers: tag_strings(tag, ItemKey::Remixer),
            dj_mixers: tag_strings(tag, ItemKey::MixDj),
            other_production: Vec::new(),
            album_artist: tag.get_string(ItemKey::AlbumArtist).map(plain_commas),
            disc_number: tag.disk(),
            disc_total: tag.disk_total(),
            disc_subtitle: tag
                .get_string(ItemKey::SetSubtitle)
                .map(ToString::to_string),
            track_number: tag.track(),
            track_total: tag.track_total(),
            date: tag
                .get_string(ItemKey::RecordingDate)
                .or_else(|| tag.get_string(ItemKey::Year))
                .map(ToString::to_string),
            // Both "ORIGINALDATE" and "ORIGINALYEAR" end up here, and the
            // year is less precise
            original_date: tag
                .get_strings(ItemKey::OriginalReleaseDate)
                .max_by_key(|s| s.len())
                .map(ToString::to_string),
            release_types: tag_strings(tag, ItemKey::MusicBrainzReleaseType),
            media: tag_strings(tag, ItemKey::OriginalMediaType),
            release_country: tag
                .get_string(ItemKey::ReleaseCountry)
                .map(ToString::to_string),
            labels: tag_strings(tag, ItemKey::Label),
            catalog_numbers: tag_strings(tag, ItemKey::CatalogNumber),
            barcodes: tag_strings(tag, ItemKey::Barcode),
            isrcs: tag_strings(tag, ItemKey::Isrc),
            genres: tag_strings(tag, ItemKey::Genre),
            composers_from_description: false,
            copyright: tag
                .get_strings(ItemKey::CopyrightMessage)
                .map(ToString::to_string)
                .collect(),
            musicbrainz: MusicBrainzIds::from_tag(tag),
            composer_ids: None,
        }
    }

    // Takes what a YouTube description has and the tags don't
    fn fill_in(&mut self, description: TrackCredits) {
        fn fill<T>(field: &mut Vec<T>, other: Vec<T>) {
            if field.is_empty() {
                *field = other;
            }
        }

        self.composers_from_description =
            self.composers.is_empty() && !description.composers.is_empty();
        fill(&mut self.composers, description.composers);
        fill(&mut self.lyricists, description.lyricists);
        fill(&mut self.arrangers, description.arrangers);
        fill(&mut self.conductors, description.conductors);
        fill(&mut self.performers, description.performers);
        fill(&mut self.producers, description.producers);
        fill(&mut self.engineers, description.engineers);
        fill(&mut self.mixers, description.mixers);
        fill(&mut self.remixers, description.remixers);
        fill(&mut self.other_production, description.other_production);
        fill(&mut self.copyright, description.copyright);

        if self.date.as_deref().is_none_or(is_upload_date) {
            self.date = description.date;
        }
        self.original_date = self.original_date.take().or(description.original_date);
    }

    // The original release date, if it's not the same as the date, e.g.
    // for reissues; the dates can be just a year
    pub fn distinct_original_date(&self) -> Option<&str> {
        let original = self.original_date.as_deref()?;
        match self.date.as_deref() {
            Some(date) if date.starts_with(original) || original.starts_with(date) => None,
            _ => Some(original),
        }
    }

    // The composers to show in the queue for a track that is not part of
    // a larger work, unless the artist already names them; only for
    // classical music, as for popular music they are the songwriters.
    // Most classical tracks from YouTube have no genre, but their
    // descriptions credit composers, while the tags of popular music
    // often do
    pub fn listed_composers(&self, artist: Option<&str>) -> Option<String> {
        let classical = self.composers_from_description
            || self
                .genres
                .iter()
                .any(|g| g.eq_ignore_ascii_case("classical"));
        let named = artist.is_some_and(|a| self.artist_is_composer(a));
        if !classical || self.composers.is_empty() || named {
            return None;
        }
        Some(self.composers.join(", "))
    }

    // A single line with the conductors and performers, skipping the
    // names that are already displayed as the artist
    // The movement, with its number if any, e.g. "2. Adagio"; only set if
    // the track is part of a larger work, as tagged by MusicBrainz Picard's
    // "Work & Movement" or "Work Hierarchy" plugins
    pub fn movement_label(&self) -> Option<String> {
        self.work.as_ref()?;
        let movement = self.movement.as_deref()?;
        match self.movement_number.as_deref() {
            Some(n) if !n.is_empty() => Some(format!("{n}. {movement}")),
            _ => Some(movement.to_string()),
        }
    }

    // The performers to show in the queue for a track that is not part of
    // a larger work, when the artist is its composer, as classical
    // releases are often tagged; the artist would be the only name shown.
    // Not when the artist performs too, as songwriters do, whose band
    // would only clutter the row
    pub fn performers_of_composer(&self, artist: Option<&str>) -> Option<String> {
        let artist = artist?;
        if self.composers.is_empty() || !self.artist_is_composer(artist) {
            return None;
        }
        if self
            .performers
            .iter()
            .any(|p| artist.contains(p.name.as_str()))
        {
            return None;
        }
        Some(self.summary(Some(artist))).filter(|s| !s.is_empty())
    }

    // Whether the artist names every composer: going by their MusicBrainz
    // ids if the tags have them, otherwise by their names
    fn artist_is_composer(&self, artist: &str) -> bool {
        match &self.composer_ids {
            Some(ids) if !ids.is_empty() && !self.musicbrainz.artists.is_empty() => {
                ids.iter().all(|id| self.musicbrainz.artists.contains(id))
            }
            _ => self.composers.iter().all(|c| names_composer(artist, c)),
        }
    }

    pub fn summary(&self, artist: Option<&str>) -> String {
        let conductors = self
            .conductors
            .iter()
            .filter(|c| Some(c.as_str()) != artist)
            .join(", ");
        let performers = self
            .performers
            .iter()
            .map(|p| p.name.as_str())
            .filter(|name| Some(*name) != artist && !self.conductors.iter().any(|c| c == name))
            .unique()
            .join(", ");

        [conductors, performers]
            .iter()
            .filter(|s| !s.is_empty())
            .join(" · ")
    }
}

// Whether an artist names a composer: in full, or by the first and last
// of three or more names, as MusicBrainz credits composers with their
// middle names or patronymics but often not the artist, e.g. "Alexander
// Borodin" for "Alexander Porfyrevich Borodin"; for tags without the ids
// of the composers
fn names_composer(artist: &str, composer: &str) -> bool {
    artist.contains(composer) || short_name(composer).is_some_and(|s| artist.contains(&s))
}

// Whether a name is the composer's, in full or shortened
fn is_composer(name: &str, composer: &str) -> bool {
    name == composer || short_name(composer).as_deref() == Some(name)
}

// "Alexander Borodin" for "Alexander Porfyrevich Borodin"
fn short_name(name: &str) -> Option<String> {
    let words = name.split_whitespace().collect::<Vec<_>>();
    match words[..] {
        [first, _, .., last] => Some(format!("{first} {last}")),
        _ => None,
    }
}

// Makes searching insensitive to case and accents, e.g. "Gemüte" becomes
// "gemute", so it can be found by typing it without the accent
pub fn normalize_search_text(s: &str) -> String {
    // The compatibility decomposition splits accented letters into the
    // letter and a combining mark, which we drop
    let decomposed = glib::normalize(s, glib::NormalizeMode::All);

    let mut res = String::with_capacity(decomposed.len());
    for c in decomposed.chars().flat_map(char::to_lowercase) {
        match c {
            '\u{0300}'..='\u{036f}'
            | '\u{1ab0}'..='\u{1aff}'
            | '\u{1dc0}'..='\u{1dff}'
            | '\u{20d0}'..='\u{20ff}'
            | '\u{fe20}'..='\u{fe2f}' => (),
            // Letters that don't decompose into a base letter
            'ß' => res.push_str("ss"),
            'æ' => res.push_str("ae"),
            'œ' => res.push_str("oe"),
            'ø' => res.push('o'),
            'ł' => res.push('l'),
            _ => res.push(c),
        }
    }

    res
}

// The text of all the tags of a track, for searching the queue; this
// leaves out the tags that only contain identifiers, numbers or technical
// data, as they would make short searches, like "op 29", match by accident
pub fn tag_search_text(tag: &Tag, extra: &ExtraComments) -> String {
    let mut text = tag
        .items()
        .filter(|item| {
            !matches!(
                item.key(),
                // The album title and artist are handled below
                ItemKey::AlbumTitle
                    | ItemKey::AlbumArtist
                    | ItemKey::AlbumArtists
                    | ItemKey::TrackNumber
                    | ItemKey::TrackTotal
                    | ItemKey::DiscNumber
                    | ItemKey::DiscTotal
                    | ItemKey::MovementNumber
                    | ItemKey::MovementTotal
                    | ItemKey::Isrc
                    | ItemKey::Barcode
                    | ItemKey::AcoustId
                    | ItemKey::AcoustIdFingerprint
                    | ItemKey::MusicBrainzRecordingId
                    | ItemKey::MusicBrainzTrackId
                    | ItemKey::MusicBrainzReleaseId
                    | ItemKey::MusicBrainzReleaseGroupId
                    | ItemKey::MusicBrainzArtistId
                    | ItemKey::MusicBrainzReleaseArtistId
                    | ItemKey::MusicBrainzWorkId
                    | ItemKey::MusicBrainzReleaseType
                    | ItemKey::ReplayGainAlbumGain
                    | ItemKey::ReplayGainAlbumPeak
                    | ItemKey::ReplayGainTrackGain
                    | ItemKey::ReplayGainTrackPeak
                    | ItemKey::EncoderSoftware
                    | ItemKey::EncoderSettings
                    | ItemKey::EncodingTime
                    | ItemKey::TaggingTime
                    | ItemKey::Length
                    | ItemKey::Bpm
                    | ItemKey::IntegerBpm
                    | ItemKey::Popularimeter
                    | ItemKey::FlagCompilation
                    | ItemKey::FlagPodcast
                    | ItemKey::AlbumTitleSortOrder
                    | ItemKey::AlbumArtistSortOrder
                    | ItemKey::TrackTitleSortOrder
                    | ItemKey::TrackArtistSortOrder
                    | ItemKey::ShowNameSortOrder
                    | ItemKey::ComposerSortOrder
            )
        })
        .filter_map(|item| item.value().text())
        // Links, like the one to the video of a download from YouTube,
        // would make searching for their site match every track from it
        .flat_map(str::split_whitespace)
        .filter(|word| !is_link(word))
        .join(" ");

    let album_artist = tag.get_string(ItemKey::AlbumArtist);
    if let Some(album_artist) = album_artist {
        text.push(' ');
        text.push_str(&album_artist_search_text(tag, album_artist));
    }

    if let Some(album) = tag.get_string(ItemKey::AlbumTitle) {
        text.push(' ');
        text.push_str(&album_title_search_text(album, album_artist));
    }

    // The names in a description from YouTube, e.g. the composer, which
    // the other tags of these tracks often lack
    if let Some(names) = description_texts(tag, extra)
        .map(youtube_description::search_names)
        .find(|names| !names.is_empty())
    {
        text.push(' ');
        text.push_str(&names.join(" "));
    }

    text
}

fn is_link(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    ["http://", "https://", "www."]
        .iter()
        .any(|prefix| word.starts_with(prefix))
}

// The tags that can hold a description generated by YouTube
fn description_texts<'a>(tag: &'a Tag, extra: &'a ExtraComments) -> impl Iterator<Item = &'a str> {
    extra
        .synopsis
        .as_deref()
        .into_iter()
        .chain(tag.get_strings(ItemKey::Description))
        .chain(tag.get_strings(ItemKey::Comment))
}

// Album titles often list the names in the album artist, e.g. the album
// "Lotti, Zelenka, Bach" by "Antonio Lotti, Jan Dismas Zelenka, Johann
// Sebastian Bach; …"; searching for one of the composers would then match
// every track of the album. Those names are already searched in the tags of
// each track, so we leave them out of the album title
fn album_title_search_text(album: &str, album_artist: Option<&str>) -> String {
    let Some(album_artist) = album_artist else {
        return album.to_string();
    };

    let names = normalize_search_text(album_artist);
    let names: std::collections::HashSet<&str> = search_words(&names).collect();

    search_words(album)
        .filter(|word| !names.contains(normalize_search_text(word).as_str()))
        .join(" ")
}

// The words of a text, without the punctuation around them
fn search_words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

// The album artist of classical releases tagged by MusicBrainz Picard lists
// the composers and then everyone who performs on the release, e.g.
// "Dieterich Buxtehude; Ton Koopman, Amsterdam Baroque Choir"; so it would
// make a search for a performer match every track of the release, even the
// ones they don't play on. We only use it to fill in for the tags a track
// doesn't have: the composers if it has no composer, and the performers if
// it has no performer or conductor, as they might only be credited there
fn album_artist_search_text(tag: &Tag, album_artist: &str) -> String {
    let (composers, performers) = match album_artist.split_once(';') {
        Some((composers, performers)) => (composers, performers),
        // Without a separator, it's the artist of the whole release
        None => ("", album_artist),
    };

    let has_composer = tag.get_string(ItemKey::Composer).is_some();
    let has_performer = tag.get_string(ItemKey::Performer).is_some()
        || tag.get_string(ItemKey::Conductor).is_some();

    let mut text = String::new();
    if !has_composer {
        text.push_str(composers);
    }
    if !has_performer {
        text.push(' ');
        text.push_str(performers);
    }

    text
}

// The Vorbis comments and APE items that lofty leaves out of the tags of
// the `TaggedFile`, as it has no `ItemKey` for them
#[derive(Debug, Default)]
pub struct ExtraComments {
    synopsis: Option<String>,
    // The JSON written by the "Work Hierarchy" plugin of MusicBrainz Picard
    work_hierarchy: Option<String>,
    // A CUE sheet that splits the file into tracks
    pub cue_sheet: Option<String>,
}

// Reads a file, and the items in `ExtraComments` if it has them
pub fn read_tagged_file(
    probe: Probe<BufReader<File>>,
    options: ParseOptions,
) -> Result<(TaggedFile, ExtraComments), FileParseError> {
    fn read<F: AudioFile + Into<TaggedFile>>(
        probe: Probe<BufReader<File>>,
        options: ParseOptions,
        comments: fn(&F) -> Option<&VorbisComments>,
    ) -> Result<(TaggedFile, ExtraComments), FileParseError> {
        let file = F::read_from(&mut probe.into_inner(), options)?;
        let get = |key| {
            comments(&file)
                .and_then(|c| c.get(key))
                .map(ToString::to_string)
        };
        let extra = ExtraComments {
            synopsis: get("SYNOPSIS"),
            work_hierarchy: get("WORK_HIERARCHY"),
            cue_sheet: get("CUESHEET"),
        };
        Ok((file.into(), extra))
    }

    // Monkey's Audio and WavPack files have their CUE sheet in an APE tag
    fn read_ape<F: AudioFile + Into<TaggedFile>>(
        probe: Probe<BufReader<File>>,
        options: ParseOptions,
        ape: fn(&F) -> Option<&ApeTag>,
    ) -> Result<(TaggedFile, ExtraComments), FileParseError> {
        let file = F::read_from(&mut probe.into_inner(), options)?;
        let cue_sheet = ape(&file)
            .and_then(|tag| tag.get("Cuesheet"))
            .and_then(|item| match item.value() {
                ItemValue::Text(text) => Some(text.clone()),
                _ => None,
            });
        let extra = ExtraComments {
            cue_sheet,
            ..Default::default()
        };
        Ok((file.into(), extra))
    }

    match probe.file_type() {
        Some(FileType::Opus) => read(probe, options, |f: &OpusFile| Some(f.vorbis_comments())),
        Some(FileType::Vorbis) => read(probe, options, |f: &VorbisFile| Some(f.vorbis_comments())),
        Some(FileType::Speex) => read(probe, options, |f: &SpeexFile| Some(f.vorbis_comments())),
        Some(FileType::Flac) => read(probe, options, FlacFile::vorbis_comments),
        Some(FileType::Ape) => read_ape(probe, options, ApeFile::ape),
        Some(FileType::WavPack) => read_ape(probe, options, WavPackFile::ape),
        _ => probe
            .options(options)
            .read()
            .map(|f| (f, ExtraComments::default())),
    }
}

// A work in a path of the "WORK_HIERARCHY" tag, from the top-level work
// down to the one the track performs
struct PathWork<'a> {
    id: &'a str,
    // The full title, e.g. "Ouvertüre nach französischer Art h-Moll, BWV
    // 831: V. Passepied I"
    title: &'a str,
    // The title without the parent's, or a number matching the position,
    // e.g. "Passepied I"
    name: &'a str,
    // The position in the parent work, if MusicBrainz has one
    position: Option<u32>,
}

impl PathWork<'_> {
    // The name with the position in front, unless the name has its own
    // number, which is the one people know: "no. 22. Chorus" is the
    // first number of Part II of Messiah
    fn numbered_name(&self) -> String {
        match self.position {
            Some(p) if !is_numbered(self.name) => format!("{p}. {}", self.name),
            _ => self.name.to_string(),
        }
    }

    // The name of a level above the movement, with the number that the
    // plugin removed from the title put back, e.g. "III. Sequentia"
    fn level_name(&self) -> String {
        let number = self
            .title
            .strip_suffix(self.name)
            .and_then(|prefix| prefix.strip_suffix(". "))
            .and_then(|prefix| prefix.rsplit([' ', ':']).next())
            .filter(|n| is_numbered(&format!("{n}. ")));
        match number {
            Some(n) => format!("{n}. {}", self.name),
            None => self.name.to_string(),
        }
    }
}

// The work and the movement to display, from the JSON written by the
// "Work Hierarchy" plugin of MusicBrainz Picard; the movement names the
// levels below the work. MusicBrainz doesn't always have the position of
// a movement, so `movement_number`, the number in the tags, is used for
// it. Returns `None` if the tags should be used instead: if the track
// performs parts of different works, or a work without parts, which may
// still be a movement in the tags if MusicBrainz didn't link it to its
// parent work
fn work_hierarchy_labels(
    json: &str,
    movement_number: Option<&str>,
) -> Option<(String, MovementName)> {
    let hierarchy = match serde_json::from_str::<serde_json::Value>(json) {
        Ok(h) => h,
        Err(e) => {
            warn!("Invalid WORK_HIERARCHY tag: {e}");
            return None;
        }
    };
    if hierarchy["version"].as_u64() != Some(1) {
        return None;
    }

    let mut paths = hierarchy["segments"]
        .as_array()?
        .iter()
        .map(|segment| {
            segment["path"]
                .as_array()?
                .iter()
                .map(|work| {
                    Some(PathWork {
                        id: work["id"].as_str()?,
                        title: work["title"].as_str()?,
                        name: work["name"].as_str()?,
                        position: work["position"]
                            .as_u64()
                            .and_then(|p| u32::try_from(p).ok()),
                    })
                })
                .collect::<Option<Vec<_>>>()
                .filter(|path| !path.is_empty())
        })
        .collect::<Option<Vec<_>>>()?;
    if let [path] = &mut paths[..]
        && let [_, .., movement] = &mut path[..]
        && movement.position.is_none()
    {
        movement.position = movement_number.and_then(|n| n.trim().parse().ok());
    }

    let top = paths.first()?.first()?;
    if paths.iter().any(|path| path[0].id != top.id) {
        return None;
    }

    let parts = paths
        .iter()
        .map(|path| &path[1..])
        .filter(|parts| !parts.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    Some((top.name.to_string(), movement_name(&parts)))
}

// The ids of the composers of the works a track performs, the last in each
// path of the "WORK_HIERARCHY" tag; `None` if the plugin that wrote it
// didn't record them. MusicBrainz often links the composer to a work but
// not to its parts, so a work without composers takes those of its
// nearest parent that has some, as the plugin does for the composer tags
fn work_hierarchy_composer_ids(json: &str) -> Option<Vec<String>> {
    let hierarchy = serde_json::from_str::<serde_json::Value>(json).ok()?;
    if hierarchy["version"].as_u64() != Some(1) {
        return None;
    }
    let mut ids = Vec::new();
    for segment in hierarchy["segments"].as_array()? {
        let path = segment["path"].as_array()?;
        let mut composers = path.last()?["composers"].as_array()?;
        if composers.is_empty() {
            // Parents that weren't looked up have no list
            if let Some(parent) = path[..path.len() - 1]
                .iter()
                .rev()
                .filter_map(|work| work["composers"].as_array())
                .find(|c| !c.is_empty())
            {
                composers = parent;
            }
        }
        for composer in composers {
            let id = composer["id"].as_str().filter(|id| is_mbid(id))?;
            if !ids.iter().any(|i| i == id) {
                ids.push(id.to_string());
            }
        }
    }
    Some(ids)
}

// The name of a movement below the top-level work: the levels above it,
// if any, e.g. "Part II", and the movement, e.g. "no. 22. Chorus"
#[derive(Debug, PartialEq)]
struct MovementName {
    levels: Option<String>,
    movement: String,
}

impl MovementName {
    fn full(&self) -> String {
        match &self.levels {
            Some(levels) => format!("{levels}: {}", self.movement),
            None => self.movement.clone(),
        }
    }
}

// The name of the movements, from the levels below the top-level work of
// each; the levels they share are named once, and the movements of the
// same part are numbered together, e.g. "Atto IV: 1, 4. “Era più calmo?”
// / “Ave Maria”"
fn movement_name(parts: &[&[PathWork]]) -> MovementName {
    let first = parts[0];
    let shared = (0..first.len() - 1)
        .take_while(|&i| {
            parts
                .iter()
                .all(|p| i + 1 < p.len() && p[i].id == first[i].id)
        })
        .count();
    let rest = parts.iter().map(|p| &p[shared..]).collect::<Vec<_>>();

    let movements = if rest.iter().all(|p| p.len() == 1) {
        let works = rest.iter().map(|p| &p[0]).collect::<Vec<_>>();
        let names = works.iter().map(|w| w.name).join(" / ");
        let positions = works.iter().map(|w| w.position).collect::<Option<Vec<_>>>();
        match positions {
            Some(p) if !works.iter().any(|w| is_numbered(w.name)) => {
                format!("{}. {names}", position_ranges(&p))
            }
            _ => names,
        }
    } else {
        rest.iter()
            .map(|p| {
                let (last, above) = p.split_last().expect("parts are not empty");
                above
                    .iter()
                    .map(PathWork::level_name)
                    .chain([last.numbered_name()])
                    .join(": ")
            })
            .join(" / ")
    };

    MovementName {
        levels: (shared > 0).then(|| first[..shared].iter().map(PathWork::level_name).join(": ")),
        movement: movements,
    }
}

// Whether a name starts with a number, e.g. "no. 22. Chorus", "Nr. 3.
// Tragödie", "LXVII. Diffusa est", "1a. Introduction", "16: Minuet" or
// "b. Benedictus"
fn is_numbered(name: &str) -> bool {
    let lower = name.to_lowercase();
    let after_no = ["no.", "no ", "nr.", "nr ", "nº", "n°", "№"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix));
    if let Some(rest) = after_no {
        return rest.trim_start().starts_with(|c: char| c.is_ascii_digit());
    }

    let end = name
        .find(|c: char| !c.is_alphanumeric())
        .unwrap_or(name.len());
    let (token, after) = name.split_at(end);
    let digits = token.trim_end_matches(|c: char| c.is_ascii_lowercase());
    if !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && token.len() - digits.len() <= 1
    {
        return after.starts_with(['.', ')', ':']);
    }
    let roman = !token.is_empty() && token.chars().all(|c| "IVXLC".contains(c));
    let letter = token.len() == 1 && token.chars().all(|c| c.is_ascii_lowercase());
    (roman || letter) && after.starts_with(['.', ')'])
}

// The positions as a list where consecutive ones are joined into a range,
// e.g. "5–6" or "1–3, 5"
fn position_ranges(positions: &[u32]) -> String {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &p in positions {
        match ranges.last_mut() {
            Some((_, end)) if p == *end + 1 => *end = p,
            _ => ranges.push((p, p)),
        }
    }

    ranges
        .iter()
        .map(|&(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}–{end}")
            }
        })
        .join(", ")
}

// yt-dlp stores the date of the upload as "YYYYMMDD", which is not when
// the track was released
fn is_upload_date(date: &str) -> bool {
    date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit())
}

// The artists of a track, if the tags name any
fn track_artist(tag: &Tag) -> Option<String> {
    let artists = tag.get_strings(ItemKey::TrackArtist).join(", ");
    (!artists.is_empty()).then(|| plain_commas(&artists))
}

// Replaces the full-width commas in a name with plain ones: tracks
// downloaded from YouTube separate their artists with commas, and use
// full-width ones for the commas within a name, e.g. "Orquesta del Teatro
// Real， Madrid", which look like large gaps
pub fn plain_commas(name: &str) -> String {
    name.replace('，', ",")
}

// All the values for a key, including the ones that were stored as a
// single value separated by semicolons
fn tag_strings(tag: &Tag, key: ItemKey) -> Vec<String> {
    tag.get_strings(key)
        .flat_map(|s| s.split(';'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unique()
        .map(ToString::to_string)
        .collect()
}

#[derive(Debug, Clone)]
pub struct TrackData {
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
    credits: TrackCredits,
    tag_text: String,
    cover: Option<AlbumCover>,
    uuid: Option<String>,
    duration: u64,
    details: FileDetails,
    // Where the track is; for a track of a CUE sheet, a path under the
    // sheet, which is not a file
    file: gio::File,
    // The part of an audio file that a track of a CUE sheet plays
    audio: Option<CueAudio>,
}

#[derive(Debug, Clone)]
pub struct CueAudio {
    pub file: gio::File,
    pub start: Duration,
    // Until the end of the file if `None`
    pub end: Option<Duration>,
    // The gains of the stream of the file are about all of it
    pub gains: Gains,
}

// The ReplayGain of a track, in dB, and its peaks
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Gains {
    pub track_gain: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain: Option<f64>,
    pub album_peak: Option<f64>,
}

impl Gains {
    fn from_tag(tag: &Tag) -> Self {
        // E.g. "-6.20 dB", or "0.988" for a peak
        let value = |key| {
            let text = tag.get_string(key)?.trim().to_ascii_lowercase();
            let number = text.strip_suffix("db").unwrap_or(&text);
            number.trim().parse::<f64>().ok().filter(|v| v.is_finite())
        };
        Self {
            track_gain: value(ItemKey::ReplayGainTrackGain),
            track_peak: value(ItemKey::ReplayGainTrackPeak),
            album_gain: value(ItemKey::ReplayGainAlbumGain),
            album_peak: value(ItemKey::ReplayGainAlbumPeak),
        }
    }
}

// The format and the properties of the file of a track
#[derive(Debug, Clone, Default)]
pub struct FileDetails {
    pub format: Option<&'static str>,
    // The path outside the sandbox, if the file came through the document
    // portal
    pub path: Option<PathBuf>,
    pub size: Option<u64>,
    // In kbps
    pub bitrate: Option<u32>,
    // In Hz
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub replay_gain: ReplayGain,
}

// The gains as written in the tags, e.g. "-6.20 dB"
#[derive(Debug, Clone, Default)]
pub struct ReplayGain {
    pub track: Option<String>,
    pub album: Option<String>,
}

impl ReplayGain {
    fn from_tag(tag: &Tag) -> Self {
        ReplayGain {
            track: tag
                .get_string(ItemKey::ReplayGainTrackGain)
                .map(ToString::to_string),
            album: tag
                .get_string(ItemKey::ReplayGainAlbumGain)
                .map(ToString::to_string),
        }
    }
}

// The usual name of a format
fn format_name(file_type: FileType) -> Option<&'static str> {
    match file_type {
        FileType::Aac => Some("AAC"),
        FileType::Aiff => Some("AIFF"),
        FileType::Ape => Some("Monkey's Audio"),
        FileType::Flac => Some("FLAC"),
        FileType::Mpeg => Some("MP3"),
        FileType::Mp4 => Some("MPEG-4 Audio"),
        FileType::Mpc => Some("Musepack"),
        FileType::Opus => Some("Opus"),
        FileType::Speex => Some("Speex"),
        FileType::Vorbis => Some("Ogg Vorbis"),
        FileType::Wav => Some("WAV"),
        FileType::WavPack => Some("WavPack"),
        _ => None,
    }
}

impl TrackData {
    pub fn artist(&self) -> Option<&str> {
        self.artist.as_deref()
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn album(&self) -> Option<&str> {
        self.album.as_deref()
    }

    pub fn tag_text(&self) -> &str {
        &self.tag_text
    }

    pub fn credits(&self) -> &TrackCredits {
        &self.credits
    }

    pub fn uuid(&self) -> Option<&str> {
        self.uuid.as_deref()
    }

    // The id of the cover, if the track has one
    pub fn cover_uuid(&self) -> Option<&str> {
        self.cover
            .as_ref()
            .filter(|cover| cover.art().is_some())
            .map(AlbumCover::uuid)
    }

    pub fn duration(&self) -> u64 {
        self.duration
    }

    pub fn details(&self) -> &FileDetails {
        &self.details
    }

    fn cover_art(&self) -> Option<&CoverArt> {
        self.cover.as_ref()?.art()
    }

    pub fn cover_texture(&self) -> Option<&gdk::Texture> {
        self.cover_art().map(CoverArt::texture)
    }

    pub fn row_cover_texture(&self) -> Option<&gdk::Texture> {
        self.cover_art().map(CoverArt::row_texture)
    }

    pub fn cover_palette(&self) -> Option<&Vec<gdk::RGBA>> {
        self.cover_art().map(CoverArt::palette)
    }

    pub fn cover_accent(&self) -> Option<gdk::RGBA> {
        self.cover_art().and_then(CoverArt::accent)
    }

    pub fn cover_cache(&self) -> Option<&PathBuf> {
        self.cover_art()?.cache()
    }

    // Reads the tracks of a file: an audio file, which a CUE sheet next to
    // it or in its tags can split into tracks, the tracks of a sheet, or a
    // track of a sheet. This blocks, so it must not be called on the main
    // thread
    fn read(uri: &str, files: &FileCache) -> Vec<Self> {
        let now = Instant::now();

        let file = gio::File::for_uri(uri);
        // Files without a local path, like the ones in the trash or on the
        // web, can't be read by lofty
        let Some(path) = file.path() else {
            warn!("Unable to find a local path for {uri}");
            return Vec::new();
        };

        let tracks = if let Some((sheet, number)) = cue_sheet::parse_location(&path) {
            let Some(sheet) = files.sheet(sheet) else {
                warn!("Unable to read the CUE sheet of {uri}");
                return Vec::new();
            };
            let Some(track) = sheet.track(number) else {
                warn!("Unable to find track {number} in {:?}", sheet.path);
                return Vec::new();
            };
            Self::read_cue_tracks(&sheet, vec![track], files)
        } else if cue_sheet::is_cue_path(&path) {
            // The files that a sheet before it in the list splits have that
            // sheet's tracks
            match files.claims.sheets.get(&path) {
                Some(sheet) => Self::read_cue_tracks(sheet, files.claims.tracks_of(&path), files),
                None => Vec::new(),
            }
        } else {
            Self::read_audio_file(file, &path, files)
        };

        debug!(
            "Tracks of {uri}: {}, loading time: {} ms",
            tracks.len(),
            now.elapsed().as_millis()
        );
        tracks
    }

    fn read_audio_file(file: gio::File, path: &Path, files: &FileCache) -> Vec<Self> {
        let (mime_type, _) = gio::content_type_guess(Some(path), None);
        if !mime_type.starts_with("audio") || mime_type.ends_with("x-mpegurl") {
            return Vec::new();
        }

        // A sheet among the files has its tracks
        if files.claims.sheet_of(path).is_some() {
            return Vec::new();
        }
        if let Some(sheet) = files.folder_sheet(path) {
            let tracks = sheet.tracks().filter(|(_, audio)| *audio == path).collect();
            return Self::read_cue_tracks(&sheet, tracks, files);
        }

        let Some(read) = FileRead::read(path) else {
            return Vec::new();
        };
        let embedded = read
            .extra
            .cue_sheet
            .as_deref()
            .and_then(|text| SplitSheet::embedded(path, text));
        match embedded {
            Some(sheet) => {
                let cue_file = CueFile::new(&sheet.sheet, &read);
                sheet
                    .tracks()
                    .map(|(track, _)| Self::cue_track(&sheet, track, &cue_file))
                    .collect()
            }
            None => vec![Self::whole_file(file, &read)],
        }
    }

    fn read_cue_tracks(
        sheet: &SplitSheet,
        tracks: Vec<(&CueTrack, &Path)>,
        files: &FileCache,
    ) -> Vec<Self> {
        tracks
            .into_iter()
            .filter_map(|(track, audio)| {
                let cue_file = files.cue_file(sheet, audio)?;
                Some(Self::cue_track(sheet, track, &cue_file))
            })
            .collect()
    }

    // A track that is a whole file
    fn whole_file(file: gio::File, read: &FileRead) -> Self {
        let path = &read.path;
        let tagged_file = &read.tagged_file;
        let extra = &read.extra;

        let mut artist = None;
        let mut title = None;
        let mut album = None;
        let mut credits = TrackCredits::default();
        let mut tag_text = String::new();
        let mut cover: Option<AlbumCover> = None;
        let mut replay_gain = ReplayGain::default();
        if let Some(tag) = tagged_file.primary_tag() {
            debug!("Found primary tag");
            artist = track_artist(tag);
            title = tag.title().map(|s| s.to_string());
            album = tag.album().map(|s| s.to_string());
            credits = TrackCredits::from_tag(tag, extra);
            replay_gain = ReplayGain::from_tag(tag);
            tag_text = tag_search_text(tag, extra);
            cover = Some(CoverCache::cover_art(path, tag));
        } else {
            warn!("Unable to load primary tag for: {path:?}");
            for tag in tagged_file.tags() {
                debug!("Found tag: {:?}", tag.tag_type());
                // The tags that come later fill in for the ones missing
                // from the first
                artist = artist.or_else(|| track_artist(tag));
                title = title.or_else(|| tag.title().map(|s| s.to_string()));
                album = album.or_else(|| tag.album().map(|s| s.to_string()));
                credits = TrackCredits::from_tag(tag, extra);
                replay_gain = ReplayGain::from_tag(tag);
                tag_text = tag_search_text(tag, extra);
                // The first tag with a cover has the one we keep
                if cover.as_ref().is_none_or(|c| c.art().is_none()) {
                    cover = Some(CoverCache::cover_art(path, tag));
                }

                if artist.is_some() && title.is_some() {
                    break;
                }
            }
        }

        let uuid = read
            .display_name
            .as_deref()
            .map(|name| track_uuid(name, None, [&artist, &title, &album]));

        TrackData {
            credits,
            tag_text,
            cover,
            uuid,
            duration: tagged_file.properties().duration().as_secs(),
            details: read.details(replay_gain),
            file,
            audio: None,
            artist,
            title,
            album,
        }
    }

    // A track of a CUE sheet, with the tags from the sheet over those of
    // its audio file
    fn cue_track(sheet: &SplitSheet, track: &CueTrack, cue_file: &CueFile) -> Self {
        let tag = cue_sheet::track_tag(&cue_file.tag, &sheet.sheet, track);
        // The other items are about the whole file
        let extra = ExtraComments::default();
        let artist = track_artist(&tag);
        let title = tag.title().map(|s| s.to_string());
        let album = tag.album().map(|s| s.to_string());

        let end = match track.end {
            Some(end) if cue_file.duration.is_zero() || end < cue_file.duration => end,
            _ => cue_file.duration,
        };
        let uuid = cue_file
            .display_name
            .as_deref()
            .map(|name| track_uuid(name, Some(track.number), [&artist, &title, &album]));
        let mut details = cue_file.details.clone();
        details.replay_gain = ReplayGain::from_tag(&tag);

        TrackData {
            credits: TrackCredits::from_tag(&tag, &extra),
            tag_text: tag_search_text(&tag, &extra),
            cover: Some(cue_file.cover.clone()),
            uuid,
            duration: end.saturating_sub(track.start).as_secs(),
            details,
            file: gio::File::for_path(sheet.location(track)),
            audio: Some(CueAudio {
                file: gio::File::for_path(&cue_file.path),
                start: track.start,
                end: track.end,
                gains: Gains::from_tag(&tag),
            }),
            artist,
            title,
            album,
        }
    }

    pub fn uri(&self) -> String {
        self.file.uri().to_string()
    }

    pub fn file(&self) -> gio::File {
        self.file.clone()
    }

    // The file that plays, which is not the track's own for the tracks of
    // CUE sheets
    pub fn audio_file(&self) -> gio::File {
        self.audio
            .as_ref()
            .map_or_else(|| self.file.clone(), |audio| audio.file.clone())
    }

    // Where the track starts and ends in its audio file
    pub fn segment(&self) -> (Duration, Option<Duration>) {
        self.audio
            .as_ref()
            .map_or((Duration::ZERO, None), |audio| (audio.start, audio.end))
    }

    // The ReplayGain of a track of a CUE sheet; the stream of a whole file
    // has its own
    pub fn gains(&self) -> Option<Gains> {
        self.audio.as_ref().map(|audio| audio.gains)
    }
}

// The id of a track, which names the cache of its waveform
fn track_uuid(display_name: &str, number: Option<u32>, tags: [&Option<String>; 3]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(display_name);
    // The tracks of a CUE sheet share their file, and can share a title
    if let Some(number) = number {
        hasher.update(format!("/track{number:02}"));
    }
    for tag in tags.into_iter().flatten() {
        hasher.update(tag);
    }
    format!("{:x}", hasher.finalize())
}

// An audio file, with its tags and properties
struct FileRead {
    path: PathBuf,
    tagged_file: TaggedFile,
    extra: ExtraComments,
    display_name: Option<String>,
    size: Option<u64>,
    // The path outside the sandbox, if the file came through the document
    // portal
    host_path: Option<PathBuf>,
}

impl FileRead {
    fn read(path: &Path) -> Option<Self> {
        let file_probe = match Probe::open(path) {
            Ok(p) => p,
            Err(e) => {
                warn!("Unable to open file {path:?}: {e}");
                return None;
            }
        };

        let (tagged_file, extra) = match file_probe.guess_file_type() {
            Ok(t) => match read_tagged_file(t, ParseOptions::new()) {
                Ok(f) => f,
                Err(e) => {
                    warn!("Unable to read parsed file {path:?}: {e}");
                    return None;
                }
            },
            Err(e) => {
                warn!("Unable to parse file type {path:?}: {e}");
                return None;
            }
        };

        let info = gio::File::for_path(path)
            .query_info(
                &format!("standard::display-name,standard::size,{ATTRIBUTE_HOST_PATH}"),
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .ok();

        Some(Self {
            path: path.to_path_buf(),
            tagged_file,
            extra,
            display_name: info.as_ref().map(|info| info.display_name().to_string()),
            size: info
                .as_ref()
                .and_then(|info| u64::try_from(info.size()).ok()),
            host_path: info
                .as_ref()
                .and_then(|info| info.attribute_as_string(ATTRIBUTE_HOST_PATH))
                .map(|p| PathBuf::from(p.as_str())),
        })
    }

    fn details(&self, replay_gain: ReplayGain) -> FileDetails {
        let properties = self.tagged_file.properties();
        FileDetails {
            format: format_name(self.tagged_file.file_type()),
            path: self.host_path.clone().or_else(|| Some(self.path.clone())),
            size: self.size,
            bitrate: properties.audio_bitrate().filter(|b| *b > 0),
            sample_rate: properties.sample_rate().filter(|r| *r > 0),
            bit_depth: properties.bit_depth().filter(|d| *d > 0),
            channels: properties.channels().filter(|c| *c > 0),
            replay_gain,
        }
    }
}

// An audio file that a CUE sheet splits, with what its tracks take from it
struct CueFile {
    path: PathBuf,
    // The tags of the album, without the pictures, which the cover is
    // read from
    tag: Tag,
    cover: AlbumCover,
    duration: Duration,
    display_name: Option<String>,
    details: FileDetails,
}

impl CueFile {
    fn new(sheet: &cue_sheet::CueSheet, read: &FileRead) -> Self {
        let file_tag = read
            .tagged_file
            .primary_tag()
            .or_else(|| read.tagged_file.first_tag());
        let mut tag = cue_sheet::album_tag(file_tag, sheet);
        // All the tracks have the same album, so they share the cover
        let cover = CoverCache::cover_art(&read.path, &tag);
        while !tag.pictures().is_empty() {
            tag.remove_picture(0);
        }
        Self {
            path: read.path.clone(),
            tag,
            cover,
            duration: read.tagged_file.properties().duration(),
            display_name: read.display_name.clone(),
            details: read.details(ReplayGain::default()),
        }
    }
}

// Values computed once, by the first thread that needs each
struct Memo<K, V>(Mutex<HashMap<K, Arc<OnceLock<V>>>>);

impl<K, V> Default for Memo<K, V> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<K: Eq + Hash, V: Clone> Memo<K, V> {
    fn get(&self, key: K, init: impl FnOnce() -> V) -> V {
        let slot = self.0.lock().unwrap().entry(key).or_default().clone();
        slot.get_or_init(init).clone()
    }
}

// What the threads reading a list of files share: which audio files the
// CUE sheets among the files split, and the sheets and the audio files
// that several tracks come from, which are read once
#[derive(Default)]
struct FileCache {
    claims: Claims,
    sheets: Memo<PathBuf, Option<Arc<SplitSheet>>>,
    // By sheet and audio file
    cue_files: Memo<(PathBuf, PathBuf), Option<Arc<CueFile>>>,
    // The ".cue" files in a folder that split audio files
    folders: Memo<PathBuf, Arc<Vec<Arc<SplitSheet>>>>,
}

impl FileCache {
    // A ".cue" file, or the sheet in the tags of an audio file
    fn sheet(&self, path: &Path) -> Option<Arc<SplitSheet>> {
        if let Some(sheet) = self.claims.sheets.get(path) {
            return Some(sheet.clone());
        }
        self.sheets.get(path.to_path_buf(), || {
            if cue_sheet::is_cue_path(path) {
                return SplitSheet::read(path).map(Arc::new);
            }
            let read = FileRead::read(path)?;
            let sheet = Arc::new(SplitSheet::embedded(
                path,
                read.extra.cue_sheet.as_deref()?,
            )?);
            // Its tracks take the rest from the file that was just read
            let key = (path.to_path_buf(), path.to_path_buf());
            self.cue_files
                .get(key, || Some(Arc::new(CueFile::new(&sheet.sheet, &read))));
            Some(sheet)
        })
    }

    fn cue_file(&self, sheet: &SplitSheet, audio: &Path) -> Option<Arc<CueFile>> {
        let key = (sheet.path.clone(), audio.to_path_buf());
        self.cue_files.get(key, || {
            let read = FileRead::read(audio)?;
            Some(Arc::new(CueFile::new(&sheet.sheet, &read)))
        })
    }

    // The first ".cue" file next to an audio file that splits it
    fn folder_sheet(&self, audio: &Path) -> Option<Arc<SplitSheet>> {
        let dir = audio.parent()?;
        self.folders
            .get(dir.to_path_buf(), || {
                Arc::new(cue_sheet::folder_sheets(dir))
            })
            .iter()
            .find(|sheet| sheet.splits(audio))
            .cloned()
    }
}

// Decoding a cover mostly waits for its sandboxed loader, so a few at a
// time go faster, even on fewer cores
const MAX_LOADING_THREADS: usize = 4;

// Reading the tags and decoding the covers takes a while, so the tracks
// are read in a few threads. They are sent in the order of the files, all
// the tracks of a file at once, which can be none; the threads stop once
// the receiver is dropped
pub fn load_tracks(files: Vec<gio::File>) -> async_channel::Receiver<Vec<TrackData>> {
    let (sender, receiver) = async_channel::unbounded();

    let uris: Arc<Vec<String>> = Arc::new(files.iter().map(|f| f.uri().to_string()).collect());
    let paths: Vec<PathBuf> = files.iter().filter_map(FileExt::path).collect();
    std::thread::spawn(move || {
        // Which sheets split the audio files, before any is read
        let cache = Arc::new(FileCache {
            claims: Claims::new(paths.iter().map(PathBuf::as_path)),
            ..Default::default()
        });

        let next = Arc::new(AtomicUsize::new(0));
        let (read_sender, read_receiver) = std::sync::mpsc::channel();
        let n_threads = MAX_LOADING_THREADS.min(uris.len()).max(1);
        for _ in 0..n_threads {
            let uris = Arc::clone(&uris);
            let next = Arc::clone(&next);
            let cache = Arc::clone(&cache);
            let read_sender = read_sender.clone();
            std::thread::spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(uri) = uris.get(index) else {
                        break;
                    };
                    // A file whose tags make the reader panic is skipped,
                    // rather than leaving the tracks after it unread
                    let data = std::panic::catch_unwind(|| TrackData::read(uri, &cache))
                        .unwrap_or_else(|_| {
                            warn!("Unable to read the tags of {uri}");
                            Vec::new()
                        });
                    if read_sender.send((index, data)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(read_sender);

        // The tracks read out of order wait for the ones before them
        let mut waiting = BTreeMap::new();
        let mut next_index = 0;
        for (index, data) in read_receiver {
            waiting.insert(index, data);
            while let Some(data) = waiting.remove(&next_index) {
                if sender.try_send(data).is_err() {
                    return;
                }
                next_index += 1;
            }
        }
    });

    receiver
}

impl Default for TrackData {
    fn default() -> Self {
        TrackData {
            artist: Some("Invalid Artist".to_string()),
            title: Some("Invalid Title".to_string()),
            album: Some("Invalid Album".to_string()),
            credits: TrackCredits::default(),
            tag_text: String::new(),
            cover: None,
            uuid: None,
            duration: 0,
            details: FileDetails::default(),
            file: gio::File::for_path("/does-not-exist"),
            audio: None,
        }
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::Track)]
    pub struct Track {
        #[property(name = "uri", get = Self::uri, type = String)]
        #[property(name = "artist", get = Self::artist, type = String)]
        #[property(name = "title", get = Self::title, type = String)]
        #[property(name = "album", get = Self::album, type = String)]
        // The conductors and performers, as a single line
        #[property(name = "credits", get = Self::credits, type = String)]
        // The main line in the track details: the work, for movements, and
        // the title otherwise
        #[property(name = "heading", get = Self::heading, type = String)]
        // The movement of a larger work, if the track is one
        #[property(name = "movement", get = Self::movement, nullable, type = Option<String>)]
        // The lines in the queue: for movements, the movement and the
        // levels of the work above it, or the performers, as the work and
        // composer are in the header of their section; otherwise the title
        // and the artist
        #[property(name = "row-title", get = Self::row_title, type = String)]
        #[property(name = "row-subtitle", get = Self::row_subtitle, type = String)]
        // A third line, for movements whose second line is the levels of
        // the work above them, with the performers, and for classical
        // tracks that are not movements, whose second line is the
        // composer, with the artist; empty otherwise
        #[property(name = "row-credits", get = Self::row_credits, type = String)]
        #[property(name = "duration", get = Self::duration, type = u64)]
        #[property(name = "cover", get = Self::cover, nullable, type = Option<gdk::Texture>)]
        // A small copy of the cover, for the rows of the lists
        #[property(name = "row-cover", get = Self::row_cover, nullable, type = Option<gdk::Texture>)]
        pub data: RefCell<TrackData>,
        #[property(get, set = Self::set_playing, explicit_notify)]
        pub playing: Cell<bool>,
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub selected: Cell<bool>,
        // Briefly, to show where the track is in a list
        #[property(get, set = Self::set_flashing, explicit_notify)]
        pub flashing: Cell<bool>,
        pub search_key: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Track {
        const NAME: &'static str = "DismasTrack";
        type Type = super::Track;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Track {}

    impl Track {
        fn uri(&self) -> String {
            self.data.borrow().uri()
        }

        fn artist(&self) -> String {
            match self.data.borrow().artist() {
                Some(artist) => artist.to_string(),
                None => i18n("Unknown artist"),
            }
        }

        fn title(&self) -> String {
            match self.data.borrow().title() {
                Some(title) => title.to_string(),
                None => i18n("Unknown title"),
            }
        }

        fn album(&self) -> String {
            match self.data.borrow().album() {
                Some(album) => album.to_string(),
                None => i18n("Unknown album"),
            }
        }

        fn credits(&self) -> String {
            let data = self.data.borrow();
            data.credits().summary(data.artist())
        }

        fn heading(&self) -> String {
            self.movement_work().unwrap_or_else(|| self.title())
        }

        fn movement(&self) -> Option<String> {
            self.data.borrow().credits().movement_label()
        }

        // The work the movement belongs to, if the track is a movement
        pub(super) fn movement_work(&self) -> Option<String> {
            self.movement()?;
            self.data.borrow().credits().work.clone()
        }

        fn row_title(&self) -> String {
            match self.movement_levels() {
                Some((_, movement)) => movement,
                None => self.movement().unwrap_or_else(|| self.title()),
            }
        }

        fn row_subtitle(&self) -> String {
            match (self.movement_levels(), self.movement()) {
                (Some((levels, _)), _) => levels,
                // The artist of classical releases is often the composer
                (None, Some(_)) => Some(self.performers())
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| self.artist()),
                (None, None) => self.listed_composers().unwrap_or_else(|| self.artist()),
            }
        }

        fn row_credits(&self) -> String {
            if self.movement_levels().is_some() {
                self.performers()
            } else if self.movement().is_some() {
                String::new()
            } else if self.listed_composers().is_some() {
                self.artist()
            } else {
                let data = self.data.borrow();
                data.credits()
                    .performers_of_composer(data.artist())
                    .unwrap_or_default()
            }
        }

        fn listed_composers(&self) -> Option<String> {
            let data = self.data.borrow();
            data.credits().listed_composers(data.artist())
        }

        // The conductors and performers, or if the tags don't have them,
        // the album artists that are not composers, e.g. "Irish Baroque
        // Orchestra, Peter Whelan" for "George Frideric Handel; Irish
        // Baroque Orchestra, Peter Whelan"
        fn performers(&self) -> String {
            let credits = self.credits();
            if !credits.is_empty() {
                return credits;
            }
            let data = self.data.borrow();
            let credits = data.credits();
            credits
                .album_artist
                .iter()
                .flat_map(|a| a.split(';'))
                .map(str::trim)
                .filter(|a| !a.is_empty() && !credits.composers.iter().any(|c| is_composer(a, c)))
                .join("; ")
        }

        // The levels of the work above the movement, and the movement
        pub(super) fn movement_levels(&self) -> Option<(String, String)> {
            self.movement()?;
            self.data.borrow().credits().movement_levels.clone()
        }

        // The composers, or the artist if the track has no composer
        pub(super) fn composer(&self) -> String {
            let composers = self.data.borrow().credits().composers.join(", ");
            if composers.is_empty() {
                self.artist()
            } else {
                composers
            }
        }

        fn duration(&self) -> u64 {
            self.data.borrow().duration()
        }

        fn cover(&self) -> Option<gdk::Texture> {
            self.data.borrow().cover_texture().cloned()
        }

        fn row_cover(&self) -> Option<gdk::Texture> {
            self.data.borrow().row_cover_texture().cloned()
        }

        fn set_playing(&self, playing: bool) {
            if self.playing.replace(playing) != playing {
                self.obj().notify_playing();
            }
        }

        fn set_selected(&self, selected: bool) {
            if self.selected.replace(selected) != selected {
                self.obj().notify_selected();
            }
        }

        fn set_flashing(&self, flashing: bool) {
            if self.flashing.replace(flashing) != flashing {
                self.obj().notify_flashing();
            }
        }
    }
}

glib::wrapper! {
    pub struct Track(ObjectSubclass<imp::Track>);
}

impl Track {
    pub fn new(data: TrackData) -> Self {
        let track: Self = glib::Object::new();
        track.imp().data.replace(data);
        track
    }

    pub fn empty() -> Self {
        glib::Object::new()
    }

    // Replaces the tags with the ones read again from the file, after it
    // changed; the track stays the same object, so it keeps its place in
    // the queue
    pub fn update(&self, data: TrackData) {
        let imp = self.imp();
        imp.data.replace(data);
        imp.search_key.take();

        self.notify_artist();
        self.notify_title();
        self.notify_album();
        self.notify_credits();
        self.notify_heading();
        self.notify_movement();
        self.notify_row_title();
        self.notify_row_subtitle();
        self.notify_row_credits();
        self.notify_duration();
        self.notify_cover();
        self.notify_row_cover();
    }

    pub fn equals(&self, other: &Self) -> bool {
        if self.uuid().is_some() && other.uuid().is_some() {
            self.uuid() == other.uuid()
        } else {
            self.uri() == other.uri()
        }
    }

    pub fn credit_details(&self) -> TrackCredits {
        self.imp().data.borrow().credits().clone()
    }

    pub fn file_details(&self) -> FileDetails {
        self.imp().data.borrow().details().clone()
    }

    pub fn cover_color(&self) -> Option<gdk::RGBA> {
        self.imp().data.borrow().cover_palette().map(|p| p[0])
    }

    pub fn cover_palette(&self) -> Option<Vec<gdk::RGBA>> {
        self.imp().data.borrow().cover_palette().cloned()
    }

    pub fn cover_accent(&self) -> Option<gdk::RGBA> {
        self.imp().data.borrow().cover_accent()
    }

    pub fn cover_uuid(&self) -> Option<String> {
        self.imp()
            .data
            .borrow()
            .cover_uuid()
            .map(ToString::to_string)
    }

    pub fn cover_cache(&self) -> Option<PathBuf> {
        self.imp().data.borrow().cover_cache().cloned()
    }

    pub fn uuid(&self) -> Option<String> {
        self.imp().data.borrow().uuid().map(ToString::to_string)
    }

    // The work the movement belongs to, if the track is a movement
    pub fn movement_work(&self) -> Option<String> {
        self.imp().movement_work()
    }

    // The levels of the work above the movement, and the movement, if the
    // queue shows them on separate lines
    pub fn movement_levels(&self) -> Option<(String, String)> {
        self.imp().movement_levels()
    }

    // The composers, or the artist if the track has no composer
    pub fn composer(&self) -> String {
        self.imp().composer()
    }

    // The text that searching the queue looks into, normalized with
    // `normalize_search_text()`; computed once, until the tags change
    pub fn search_key(&self) -> String {
        self.imp()
            .search_key
            .borrow_mut()
            .get_or_insert_with(|| {
                let data = self.imp().data.borrow();
                // The album title is in the tags, without the names that
                // would make it match tracks by other people
                let key = [self.artist(), self.title(), data.tag_text().to_string()].join(" ");
                normalize_search_text(&key)
            })
            .clone()
    }

    pub fn file(&self) -> gio::File {
        self.imp().data.borrow().file()
    }

    pub fn audio_file(&self) -> gio::File {
        self.imp().data.borrow().audio_file()
    }

    pub fn audio_uri(&self) -> String {
        self.audio_file().uri().to_string()
    }

    pub fn segment(&self) -> (Duration, Option<Duration>) {
        self.imp().data.borrow().segment()
    }

    pub fn gains(&self) -> Option<Gains> {
        self.imp().data.borrow().gains()
    }
}

impl Default for Track {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use gtk::gio::prelude::*;
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::{
        Credit, ExtraComments, MovementName, MusicBrainzIds, TrackCredits, album_title_search_text,
        is_numbered, normalize_search_text, position_ranges, tag_search_text,
        work_hierarchy_composer_ids, work_hierarchy_labels,
    };

    #[test]
    fn search_text_has_description_names_but_no_links() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::TrackTitle, "Aqua".to_string());
        tag.insert_text(
            ItemKey::Comment,
            "https://www.youtube.com/watch?v=LLIsy64Sv5c".to_string(),
        );
        let extra = ExtraComments {
            synopsis: Some(
                "Provided to YouTube by IIP-DDS\n\nAqua · Núria Rial\n\nAqua\n\n\
                 Composer: Núria Rial\n\nAuto-generated by YouTube."
                    .to_string(),
            ),
            ..Default::default()
        };
        let text = normalize_search_text(&tag_search_text(&tag, &extra));
        assert!(text.contains("nuria rial"));
        assert!(!text.contains("youtube"));
        assert!(text.contains("aqua"));
    }

    fn dates(date: Option<&str>, original_date: Option<&str>) -> TrackCredits {
        TrackCredits {
            date: date.map(str::to_string),
            original_date: original_date.map(str::to_string),
            ..Default::default()
        }
    }

    type Work<'a> = (&'a str, &'a str, Option<u32>);

    // A "WORK_HIERARCHY" tag with a segment for each path, given as the
    // id, name and position of each work
    fn hierarchy(paths: &[&[Work]]) -> String {
        let segments = paths
            .iter()
            .map(|path| {
                let path = path
                    .iter()
                    .map(|(id, name, position)| {
                        serde_json::json!({
                            "id": id, "title": name, "name": name, "position": position,
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({ "path": path, "attributes": [] })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "version": 1, "segments": segments }).to_string()
    }

    fn labels(paths: &[&[Work]]) -> Option<(String, String)> {
        work_hierarchy_labels(&hierarchy(paths), None).map(|(work, m)| (work, m.full()))
    }

    fn movement(paths: &[&[Work]]) -> Option<String> {
        labels(paths).map(|(_, movement)| movement)
    }

    const SUITE: Work = ("bwv831", "Ouvertüre, BWV 831", None);
    const MESSIAH: Work = ("messiah", "Messiah, HWV 56", None);

    #[test]
    fn work_hierarchy_numbers_the_movements() {
        assert_eq!(
            labels(&[&[SUITE, ("v", "Passepied I", Some(5))]]),
            Some((
                "Ouvertüre, BWV 831".to_string(),
                "5. Passepied I".to_string()
            ))
        );
        assert_eq!(
            movement(&[
                &[SUITE, ("v", "Passepied I", Some(5))],
                &[SUITE, ("vi", "Passepied II", Some(6))],
            ])
            .as_deref(),
            Some("5–6. Passepied I / Passepied II")
        );
    }

    #[test]
    fn work_hierarchy_falls_back_on_the_movement_tag() {
        let concerto = ("op68", "Violin Concerto in D major, op. 68", None);
        let json = hierarchy(&[&[concerto, ("ii", "Andante", None)]]);
        assert_eq!(
            work_hierarchy_labels(&json, Some("2"))
                .map(|(_, m)| m.full())
                .as_deref(),
            Some("2. Andante")
        );
        assert_eq!(
            work_hierarchy_labels(&json, None)
                .map(|(_, m)| m.full())
                .as_deref(),
            Some("Andante")
        );
        // MusicBrainz's position is preferred
        let json = hierarchy(&[&[concerto, ("ii", "Andante", Some(3))]]);
        assert_eq!(
            work_hierarchy_labels(&json, Some("2"))
                .map(|(_, m)| m.full())
                .as_deref(),
            Some("3. Andante")
        );
    }

    #[test]
    fn work_hierarchy_puts_back_the_numbers_of_levels() {
        let json = r#"{"version":1,"segments":[{"path":[
            {"id":"r","title":"Requiem, op. 89","name":"Requiem, op. 89","position":null},
            {"id":"1","title":"Requiem, op. 89: I. oddíl","name":"oddíl","position":1},
            {"id":"3","title":"Requiem, op. 89: I. oddíl: III. Sequentia","name":"Sequentia","position":3},
            {"id":"q","title":"Requiem, op. 89: I. oddíl: III. Sequentia: Quid sum miser","name":"Quid sum miser","position":3}
        ]}]}"#;
        assert_eq!(
            work_hierarchy_labels(json, None)
                .map(|(_, m)| m.full())
                .as_deref(),
            Some("I. oddíl: III. Sequentia: 3. Quid sum miser")
        );
    }

    #[test]
    fn work_hierarchy_numbers_the_lowest_level() {
        let wtc = ("wtc", "The Well-Tempered Clavier", None);
        let book = ("book1", "Book I", Some(1));
        let pf = (
            "pf12",
            "Prelude and Fugue no. 12 in F minor, BWV 857",
            Some(12),
        );
        assert_eq!(
            movement(&[&[wtc, book, pf, ("p", "Prelude", Some(1))]]).as_deref(),
            Some("Book I: Prelude and Fugue no. 12 in F minor, BWV 857: 1. Prelude")
        );

        // The levels in common are only named once
        let otello = ("otello", "Otello", None);
        let act = ("act4", "Atto IV", Some(5));
        assert_eq!(
            movement(&[
                &[otello, act, ("a", "“Era più calmo?”", Some(1))],
                &[otello, act, ("b", "“Ave Maria”", Some(4))],
            ])
            .as_deref(),
            Some("Atto IV: 1, 4. “Era più calmo?” / “Ave Maria”")
        );
        let opera = ("opera", "Opera", None);
        assert_eq!(
            movement(&[
                &[opera, ("act1", "Act I", Some(1)), ("f", "Finale", Some(9))],
                &[
                    opera,
                    ("act2", "Act II", Some(2)),
                    ("i", "Introduction", Some(1))
                ],
            ])
            .as_deref(),
            Some("Act I: 9. Finale / Act II: 1. Introduction")
        );
    }

    #[test]
    fn work_hierarchy_separates_the_levels_above_the_movement() {
        let name = |paths: &[&[Work]]| work_hierarchy_labels(&hierarchy(paths), None).unwrap().1;
        let wtc = ("wtc", "The Well-Tempered Clavier", None);
        let book = ("book1", "Book I", Some(1));
        let pf = ("pf1", "Prelude and Fugue no. 1", Some(1));
        assert_eq!(
            name(&[&[wtc, book, pf, ("p", "Prelude", Some(1))]]),
            MovementName {
                levels: Some("Book I: Prelude and Fugue no. 1".to_string()),
                movement: "1. Prelude".to_string(),
            }
        );
        assert_eq!(
            name(&[&[SUITE, ("v", "Passepied I", Some(5))]]),
            MovementName {
                levels: None,
                movement: "5. Passepied I".to_string(),
            }
        );
        // Only the levels in common are above the movements
        let opera = ("opera", "Opera", None);
        assert_eq!(
            name(&[
                &[opera, ("act1", "Act I", Some(1)), ("f", "Finale", Some(9))],
                &[
                    opera,
                    ("act2", "Act II", Some(2)),
                    ("i", "Introduction", Some(1))
                ],
            ]),
            MovementName {
                levels: None,
                movement: "Act I: 9. Finale / Act II: 1. Introduction".to_string(),
            }
        );
    }

    #[test]
    fn work_hierarchy_keeps_the_numbers_in_names() {
        let part = ("part2", "Part II", Some(2));
        let chorus = ("22", "no. 22. Chorus “Behold the Lamb of God”", Some(1));
        assert_eq!(
            movement(&[&[MESSIAH, part, chorus]]).as_deref(),
            Some("Part II: no. 22. Chorus “Behold the Lamb of God”")
        );
        // Without a position, there is no number
        assert_eq!(
            movement(&[&[MESSIAH, part, ("x", "Chorus", None)]]).as_deref(),
            Some("Part II: Chorus")
        );
    }

    #[test]
    fn work_hierarchy_is_ignored_when_it_does_not_fit() {
        // Parts of different works, e.g. the same movement in two
        // catalogues
        assert_eq!(
            labels(&[
                &[("t1", "Mass, T 1", None), ("a", "Benedictus", Some(5))],
                &[("p31", "Mass, P. 31", None), ("b", "Benedictus", Some(4))],
            ]),
            None
        );
        // A work without parts, as the tags may know its parent work
        assert_eq!(labels(&[&[("song", "Song", None)]]), None);
        assert_eq!(work_hierarchy_labels("not json", None), None);
        assert_eq!(
            work_hierarchy_labels(r#"{"version":2,"segments":[]}"#, None),
            None
        );
        assert_eq!(
            work_hierarchy_labels(r#"{"version":1,"segments":[]}"#, None),
            None
        );
    }

    #[test]
    fn work_hierarchy_replaces_the_work_tags() {
        // The plugin can tag the parent as the work, e.g. "Messiah, HWV
        // 56: Part II", but the queue groups by the top-level work
        let part = ("part2", "Part II", Some(2));
        let chorus = ("22", "no. 22. Chorus", Some(1));
        let (work, movement) = labels(&[&[MESSIAH, part, chorus]]).unwrap();
        let credits = TrackCredits {
            work: Some(work),
            movement: Some(movement),
            ..Default::default()
        };
        assert_eq!(credits.work.as_deref(), Some("Messiah, HWV 56"));
        assert_eq!(
            credits.movement_label().as_deref(),
            Some("Part II: no. 22. Chorus")
        );
    }

    #[test]
    fn names_with_their_own_numbers() {
        for name in [
            "no. 22. Chorus",
            "No. 9 Interlude",
            "Nr. 3. Der arme Peter",
            "28. Recitativo",
            "1a. Introduction",
            "16: (Minuet)",
            "LXVII. Diffusa est gratia",
            "b. Benedictus",
            "IV) Finale",
        ] {
            assert!(is_numbered(name), "{name}");
        }
        for name in [
            "Prelude",
            "Nocturne",
            "No vale la pena",
            "1812 Overture",
            "Credo",
            "Vivace",
            "I lacci tende più forti",
            "Scena I. (no. 1) Duetto",
            "D. 960",
        ] {
            assert!(!is_numbered(name), "{name}");
        }
    }

    #[test]
    fn composers_are_listed_for_classical_music() {
        let brentner = TrackCredits {
            composers: vec!["Jan Josef Ignác Brentner".to_string()],
            composers_from_description: true,
            ..Default::default()
        };
        assert_eq!(
            brentner
                .listed_composers(Some("Inégal Ensemble, Adam Viktora"))
                .as_deref(),
            Some("Jan Josef Ignác Brentner")
        );
        // Not if the artist names them already
        assert_eq!(
            brentner.listed_composers(Some("Jan Josef Ignác Brentner, Collegium Marianum")),
            None
        );

        let song = TrackCredits {
            composers: vec!["John Lennon".to_string(), "Paul McCartney".to_string()],
            ..Default::default()
        };
        assert_eq!(song.listed_composers(Some("The Beatles")), None);
        let classical = TrackCredits {
            genres: vec!["Baroque".to_string(), "Classical".to_string()],
            ..song
        };
        assert_eq!(
            classical.listed_composers(Some("The Beatles")).as_deref(),
            Some("John Lennon, Paul McCartney")
        );

        // Nor if the artist names them without their middle names
        let borodin = TrackCredits {
            composers: vec!["Alexander Porfyrevich Borodin".to_string()],
            genres: vec!["Classical".to_string()],
            ..Default::default()
        };
        assert_eq!(borodin.listed_composers(Some("Alexander Borodin")), None);
        assert_eq!(
            borodin
                .listed_composers(Some("Alexander Glazunov"))
                .as_deref(),
            Some("Alexander Porfyrevich Borodin")
        );
    }

    #[test]
    fn composer_ids_are_read_from_the_performed_works() {
        let json = serde_json::json!({
            "version": 1,
            "segments": [
                { "path": [
                    { "id": "w1", "composers": [{ "id": "aaaa", "name": "A" }] },
                    { "id": "w2", "composers": [{ "id": "bbbb", "name": "B" }] },
                ] },
                { "path": [
                    { "id": "w3", "composers": [
                        { "id": "bbbb", "name": "B" }, { "id": "cccc", "name": "C" },
                    ] },
                ] },
            ],
        });
        assert_eq!(
            work_hierarchy_composer_ids(&json.to_string()),
            Some(vec!["bbbb".to_string(), "cccc".to_string()])
        );

        // Tags written before the plugin recorded composers
        let json = r#"{"version":1,"segments":[{"path":[{"id":"w1"}]}]}"#;
        assert_eq!(work_hierarchy_composer_ids(json), None);
        // Or by a lookup that failed
        let json = r#"{"version":1,"segments":[{"path":[{"id":"w1","composers":null}]}]}"#;
        assert_eq!(work_hierarchy_composer_ids(json), None);
    }

    #[test]
    fn composer_ids_are_inherited_from_the_nearest_parent() {
        let json = serde_json::json!({
            "version": 1,
            "segments": [
                { "path": [
                    { "id": "opera", "composers": [{ "id": "aaaa" }] },
                    { "id": "act", "composers": [{ "id": "bbbb" }] },
                    { "id": "scene", "composers": null },
                    { "id": "number", "composers": [] },
                ] },
                // A work with its own composers keeps them
                { "path": [
                    { "id": "suite", "composers": [{ "id": "aaaa" }] },
                    { "id": "dance", "composers": [{ "id": "cccc" }] },
                ] },
                // And one with none anywhere adds nothing
                { "path": [{ "id": "song", "composers": [] }] },
            ],
        });
        assert_eq!(
            work_hierarchy_composer_ids(&json.to_string()),
            Some(vec!["bbbb".to_string(), "cccc".to_string()])
        );
    }

    #[test]
    fn composer_ids_decide_whether_the_artist_is_the_composer() {
        // The names alone would say the son is his father
        let father = TrackCredits {
            composers: vec!["Johann Strauss".to_string()],
            genres: vec!["Classical".to_string()],
            composer_ids: Some(vec!["1111".to_string()]),
            musicbrainz: MusicBrainzIds {
                artists: vec!["2222".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            father
                .listed_composers(Some("Johann Strauss II"))
                .as_deref(),
            Some("Johann Strauss")
        );

        // And that the names don't match
        let tchaikovsky = TrackCredits {
            composers: vec!["Pyotr Ilyich Tchaikovsky".to_string()],
            genres: vec!["Classical".to_string()],
            performers: vec![Credit::parse("Sviatoslav Richter (piano)")],
            composer_ids: Some(vec!["1111".to_string()]),
            musicbrainz: MusicBrainzIds {
                artists: vec!["1111".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            tchaikovsky.listed_composers(Some("P. I. Tchaikovsky")),
            None
        );
        assert_eq!(
            tchaikovsky
                .performers_of_composer(Some("P. I. Tchaikovsky"))
                .as_deref(),
            Some("Sviatoslav Richter")
        );
    }

    #[test]
    fn performers_are_listed_when_the_artist_is_the_composer() {
        let arabeske = TrackCredits {
            composers: vec!["Robert Schumann".to_string()],
            performers: vec![Credit::parse("Fabrizio Chiovetta (piano)")],
            ..Default::default()
        };
        assert_eq!(
            arabeske
                .performers_of_composer(Some("Robert Schumann"))
                .as_deref(),
            Some("Fabrizio Chiovetta")
        );
        // Not if the artist isn't the composer
        assert_eq!(
            arabeske.performers_of_composer(Some("Fabrizio Chiovetta")),
            None
        );

        // Nor if the artist performs, like a songwriter with a band
        let song = TrackCredits {
            composers: vec!["Joni Mitchell".to_string()],
            performers: vec![
                Credit::parse("Joni Mitchell (vocals)"),
                Credit::parse("Jaco Pastorius (bass)"),
            ],
            ..Default::default()
        };
        assert_eq!(song.performers_of_composer(Some("Joni Mitchell")), None);

        // Also when the artist leaves out the composer's patronymic
        let trio = TrackCredits {
            composers: vec!["Alexander Porfyrevich Borodin".to_string()],
            performers: vec![
                Credit::parse("Alexander Detisov (violin)"),
                Credit::parse("Alexander Osokin (cello)"),
            ],
            ..Default::default()
        };
        assert_eq!(
            trio.performers_of_composer(Some("Alexander Borodin"))
                .as_deref(),
            Some("Alexander Detisov, Alexander Osokin")
        );
    }

    #[test]
    fn position_ranges_join_consecutive_positions() {
        assert_eq!(position_ranges(&[5]), "5");
        assert_eq!(position_ranges(&[5, 6]), "5–6");
        assert_eq!(position_ranges(&[1, 2, 3, 5]), "1–3, 5");
        assert_eq!(position_ranges(&[6, 5]), "6, 5");
    }

    #[test]
    fn original_date_is_only_shown_if_different() {
        let reissue = dates(Some("2007-03-01"), Some("1961"));
        assert_eq!(reissue.distinct_original_date(), Some("1961"));
        assert_eq!(
            dates(Some("2016-06-24"), Some("2016")).distinct_original_date(),
            None
        );
        assert_eq!(
            dates(Some("2016"), Some("2016-06-24")).distinct_original_date(),
            None
        );
        assert_eq!(
            dates(None, Some("1961")).distinct_original_date(),
            Some("1961")
        );
        assert_eq!(dates(Some("2007"), None).distinct_original_date(), None);
    }

    #[test]
    fn description_replaces_only_the_upload_date() {
        let description = || dates(Some("2019-03-01"), Some("1963"));

        let mut downloaded = dates(Some("20240411"), None);
        downloaded.fill_in(description());
        assert_eq!(downloaded.date.as_deref(), Some("2019-03-01"));
        assert_eq!(downloaded.original_date.as_deref(), Some("1963"));

        let mut tagged = dates(Some("2019"), Some("1962"));
        tagged.fill_in(description());
        assert_eq!(tagged.date.as_deref(), Some("2019"));
        assert_eq!(tagged.original_date.as_deref(), Some("1962"));
    }

    #[test]
    fn album_title_leaves_out_album_artist_names() {
        let artist = "Antonio Lotti, Jan Dismas Zelenka, Johann Sebastian Bach; Thomas Hengelbrock, Balthasar‐Neumann‐Chor";
        assert_eq!(
            album_title_search_text("Lotti, Zelenka, Bach", Some(artist)),
            ""
        );
        assert_eq!(
            album_title_search_text(
                "Images: Hommage à Marcel Tournier",
                Some("Marcel Tournier; Emmanuel Ceysson")
            ),
            "Images Hommage à"
        );
        assert_eq!(
            album_title_search_text(
                "Cantatas, Volume 12",
                Some("Johann Sebastian Bach; Ton Koopman")
            ),
            "Cantatas Volume 12"
        );
        assert_eq!(
            album_title_search_text("Il giardino di rose", None),
            "Il giardino di rose"
        );
    }

    #[test]
    fn search_text_ignores_case_and_accents() {
        assert_eq!(
            normalize_search_text("Tröste mir, Jesu, mein Gemüte"),
            "troste mir, jesu, mein gemute"
        );
        assert_eq!(normalize_search_text("Antonín Dvořák"), "antonin dvorak");
        assert_eq!(normalize_search_text("L’Étang du Parc"), "l’etang du parc");
        assert_eq!(
            normalize_search_text("Kantate „Ach Gott“"),
            "kantate „ach gott“"
        );
    }

    #[test]
    fn search_text_expands_letters_without_decomposition() {
        assert_eq!(normalize_search_text("Groß"), "gross");
        assert_eq!(
            normalize_search_text("Œuvres complètes"),
            "oeuvres completes"
        );
        assert_eq!(normalize_search_text("Søren Łukasz"), "soren lukasz");
    }

    // The tracks read from some files, as the queue gets them
    fn read_tracks(files: &[&std::path::Path]) -> Vec<Vec<super::TrackData>> {
        use super::{Claims, FileCache, TrackData};

        let cache = FileCache {
            claims: Claims::new(files.iter().copied()),
            ..Default::default()
        };
        files
            .iter()
            .map(|path| {
                let uri = gtk::gio::File::for_path(path).uri();
                TrackData::read(&uri, &cache)
            })
            .collect()
    }

    #[test]
    fn a_cue_sheet_splits_its_file_into_tracks() {
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!("dismas-cue-tracks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("Album.wav");
        crate::utils::write_test_wav(&wav, 10);
        let cue = dir.join("Album.cue");
        std::fs::write(
            &cue,
            "PERFORMER \"Kino\"\nTITLE \"Album\"\nFILE \"Album.wav\" WAVE\n\
             TRACK 01 AUDIO\nTITLE \"One\"\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nTITLE \"Two\"\nINDEX 01 00:03:00\n\
             TRACK 03 AUDIO\nTITLE \"Three\"\nPERFORMER \"Guest\"\nINDEX 01 00:07:00\n",
        )
        .unwrap();

        // Listed together, the sheet has the tracks, and the file none
        let read = read_tracks(&[&cue, &wav]);
        assert!(read[1].is_empty());
        let tracks = &read[0];
        let titles: Vec<_> = tracks.iter().map(|t| t.title().unwrap()).collect();
        assert_eq!(titles, vec!["One", "Two", "Three"]);
        let durations: Vec<_> = tracks.iter().map(super::TrackData::duration).collect();
        assert_eq!(durations, vec![3, 4, 3]);
        assert_eq!(tracks[1].artist(), Some("Kino"));
        assert_eq!(tracks[2].artist(), Some("Guest"));
        assert_eq!(tracks[2].album(), Some("Album"));
        assert_eq!(tracks[2].credits().track_number, Some(3));
        assert_eq!(tracks[1].file().path().unwrap(), cue.join("track02"));
        assert_eq!(tracks[1].audio_file().path().unwrap(), wav);
        assert_eq!(
            tracks[1].segment(),
            (Duration::from_secs(3), Some(Duration::from_secs(7)))
        );
        assert_eq!(tracks[2].segment(), (Duration::from_secs(7), None));
        assert_eq!(tracks[1].details().path.as_deref(), Some(wav.as_path()));
        assert_ne!(tracks[0].uuid(), tracks[1].uuid());

        // Alone, the file is split by the sheet next to it, with the same
        // tracks
        let alone = read_tracks(&[&wav]).remove(0);
        let locations: Vec<_> = alone.iter().map(super::TrackData::uri).collect();
        let expected: Vec<_> = tracks.iter().map(super::TrackData::uri).collect();
        assert_eq!(locations, expected);

        // A track of the sheet, as a playlist has it
        let track = read_tracks(&[&cue.join("track02")]).remove(0);
        assert_eq!(track.len(), 1);
        assert_eq!(track[0].title(), Some("Two"));

        // Without the sheet, the file is a track
        std::fs::remove_file(&cue).unwrap();
        let whole = read_tracks(&[&wav]).remove(0);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].duration(), 10);
        assert_eq!(whole[0].segment(), (Duration::ZERO, None));
        assert!(read_tracks(&[&cue.join("track02")])[0].is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
