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
    id3::v2::Id3v2Tag,
    mp4::{AtomData, AtomIdent, Ilst, Mp4File},
    mpeg::MpegFile,
    ogg::{OpusFile, SpeexFile, VorbisFile, tag::VorbisComments},
    prelude::{Accessor, TaggedFileExt},
    probe::Probe,
    tag::{ItemKey, ItemValue, Tag},
    wavpack::WavPackFile,
};
use log::{debug, warn};
use sha2::{Digest, Sha256};

use crate::{
    audio::cover_cache::{AlbumCover, CoverArt, CoverCache},
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::{i18n, i18n_f},
    library::{folder_album_title, is_mbid},
    utils::{self, ATTRIBUTE_HOST_PATH},
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

    // A soloist, by the role MusicBrainz gives one, e.g. "solo violin",
    // "soprano solo" or "soloist"
    fn is_soloist(&self) -> bool {
        self.role
            .as_deref()
            .is_some_and(|r| r.to_lowercase().contains("solo"))
    }

    // A singer, e.g. "soprano vocals", but not a choir
    fn is_singer(&self) -> bool {
        self.role
            .as_deref()
            .is_some_and(|r| r.ends_with("vocals") && !GROUP_ROLES.contains(&r))
    }
}

// The roles Picard gives a group rather than a player, e.g. "La
// Serenissima (orchestra)" or "Vogler Quartett (string quartet)". It gives
// "instruments" to both, so that one is left to `TrackCredits::is_group`
const GROUP_ROLES: [&str; 6] = [
    "orchestra",
    "string quartet",
    "ensemble",
    "musical ensemble",
    "choir",
    "choir vocals",
];

// The additional credits and release information in the tags, mostly
// relevant for classical music
#[derive(Debug, Clone, Default)]
pub struct TrackCredits {
    pub work: Option<String>,
    // The full title of the work, when it's part of a larger one, e.g.
    // "The Well-Tempered Clavier, Book I: Prelude and Fugue no. 1 in C
    // major, BWV 846"
    pub work_title: Option<String>,
    pub movement: Option<String>,
    pub movement_number: Option<String>,
    // The levels of the work above the movement, and the movement, when
    // the movement is part of a part of the work, e.g. "Book I: Prelude
    // and Fugue no. 1 in C major" and "1. Prelude"; the queue shows them
    // on separate lines, as the movement would be cut off
    pub movement_levels: Option<(String, String)>,
    pub composers: Vec<String>,
    // The composers of the work, from the "Work Hierarchy" plugin; empty
    // if it didn't record them. Its movements can credit others too, e.g.
    // the author of the hymn in a chorale of a Bach cantata, or Süßmayr in
    // the movements of Mozart's Requiem he completed
    pub work_composers: Vec<String>,
    pub lyricists: Vec<String>,
    pub arrangers: Vec<String>,
    pub conductors: Vec<String>,
    pub performers: Vec<Credit>,
    pub producers: Vec<String>,
    pub engineers: Vec<String>,
    pub mixers: Vec<String>,
    pub remixers: Vec<String>,
    pub dj_mixers: Vec<String>,
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
    // The credits in the tags; lofty leaves the tags it doesn't know out of
    // `tag`, so `extra` has the ones we need
    fn from_tag(tag: &Tag, extra: &ExtraComments) -> Self {
        let mut credits = Self::from_tag_items(tag);
        credits.composer_ids = extra
            .work_hierarchy
            .as_deref()
            .and_then(work_hierarchy_composer_ids);
        // The hierarchy has the numbers of every level, so the movement
        // includes them where they belong, whatever the work and movement
        // tags say. A track of a whole part with a catalogue number of its
        // own, a Prelude and Fugue in one track, is not a movement
        if let Some(WorkLabels {
            work,
            title,
            movement,
            composers,
        }) = extra
            .work_hierarchy
            .as_deref()
            .and_then(work_hierarchy_labels)
        {
            credits.work_title = (title != work).then_some(title);
            credits.work = Some(work);
            credits.work_composers = composers;
            credits.movement = movement.as_ref().map(MovementName::full);
            credits.movement_number = None;
            credits.movement_levels =
                movement.and_then(|movement| Some((movement.levels?, movement.movement)));
        }

        credits
    }

    fn from_tag_items(tag: &Tag) -> Self {
        TrackCredits {
            work: tag.get_string(ItemKey::Work).map(ToString::to_string),
            work_title: None,
            movement: tag.get_string(ItemKey::Movement).map(ToString::to_string),
            movement_number: tag
                .get_string(ItemKey::MovementNumber)
                .map(ToString::to_string),
            movement_levels: None,
            composers: tag_strings(tag, ItemKey::Composer),
            work_composers: Vec::new(),
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
            copyright: tag
                .get_strings(ItemKey::CopyrightMessage)
                .map(ToString::to_string)
                .collect(),
            musicbrainz: MusicBrainzIds::from_tag(tag),
            composer_ids: None,
        }
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
    // classical music, as for popular music they are the songwriters
    pub fn listed_composers(&self, artist: Option<&str>) -> Option<String> {
        let classical = self
            .genres
            .iter()
            .any(|g| g.eq_ignore_ascii_case("classical"));
        let named = artist.is_some_and(|a| self.artist_is_composer(a));
        if !classical || self.composers.is_empty() || named {
            return None;
        }
        Some(self.composers.join(", "))
    }

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
    // They come from the tags, or else from the album artist. Not when the
    // artist performs too, as songwriters do, whose band would only
    // clutter the row
    pub fn performers_of_composer(&self, artist: Option<&str>) -> Option<String> {
        let artist = artist?;
        // Without composers in the tags, the album artist tells whether the
        // artist is the composer
        let is_composer = if self.composers.is_empty() {
            self.album_performers_of_composer(artist).is_some()
        } else {
            self.artist_is_composer(artist)
        };
        if !is_composer
            || self
                .performers
                .iter()
                .any(|p| artist.contains(p.name.as_str()))
        {
            return None;
        }
        Some(self.summary(Some(artist)))
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.album_performers_of_composer(artist)
                    .map(ToString::to_string)
            })
    }

    // The performers in the album artist, when the artist is one of the
    // composers it lists before them, as MusicBrainz Picard tags classical
    // releases, e.g. "Kammerchor Stuttgart & Frieder Bernius" for "Johannes
    // Brahms" and "Johannes Brahms; Kammerchor Stuttgart & Frieder Bernius".
    // The performers are after the last semicolon, as composers can be
    // separated by semicolons too, e.g. "František Ignác Tůma; Jan Dismas
    // Zelenka; Collegium 1704, Václav Luks". Not when the artist is among
    // the performers, as a composer playing their own music would be shown
    // twice
    fn album_performers_of_composer(&self, artist: &str) -> Option<&str> {
        let (composers, performers) = self.album_artist.as_deref()?.rsplit_once(';')?;
        let performers = performers.trim();
        (composers.contains(artist) && !performers.is_empty() && !performers.contains(artist))
            .then_some(performers)
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

    // Whether the album artist names someone, as a release credits there
    // its soloists and the groups that play it
    fn album_artist_names(&self, name: &str) -> bool {
        self.album_artist
            .as_deref()
            .is_some_and(|a| a.contains(name))
    }

    // Whether a performer is a group rather than a player. Picard gives
    // "instruments" to both, e.g. "The Purcell Quartet (instruments)" and
    // a single player of the Royal Swedish Navy Band, so it only counts
    // when the album artist names the performer
    fn is_group(&self, performer: &Credit) -> bool {
        match performer.role.as_deref() {
            Some("instruments") => self.album_artist_names(&performer.name),
            Some(role) => GROUP_ROLES.contains(&role),
            None => false,
        }
    }

    // The performers to name in the credits line. Picard lists every
    // player of a group with it, e.g. the nineteen of "La Serenissima
    // (orchestra)" or the four of "Vogler Quartett (string quartet)", so
    // with a group, only the soloists are named, before the groups: those
    // marked solo, those the album artist names, and the singers if there
    // are at most four, like the soloists of a mass, as the members of a
    // vocal ensemble are credited as singers too
    fn featured_performers(&self) -> Vec<&Credit> {
        if !self.performers.iter().any(|p| self.is_group(p)) {
            return self.performers.iter().collect();
        }
        let singers = self
            .performers
            .iter()
            .filter(|p| p.is_singer())
            .map(|p| &p.name)
            .unique()
            .count();
        let (groups, soloists): (Vec<_>, Vec<_>) = self
            .performers
            .iter()
            .filter(|p| {
                self.is_group(p)
                    || p.is_soloist()
                    || self.album_artist_names(&p.name)
                    || (p.is_singer() && singers <= 4)
            })
            .partition(|p| self.is_group(p));
        soloists.into_iter().chain(groups).collect()
    }

    // The performers in the album artist, in place of a list of more than
    // five players with no group, e.g. "Ensemble Zefiro, Alfredo
    // Bernardini" for the fourteen players of a wind band on "Wolfgang
    // Amadeus Mozart; Ensemble Zefiro, Alfredo Bernardini"; the players of
    // a quintet still fit in the line, and larger groups are usually
    // orchestras. They're after the last semicolon, or the whole album
    // artist if it has none and doesn't name a composer, e.g. "Sol
    // Gabetta"
    fn ensemble_in_album_artist(&self) -> Option<&str> {
        let players = self.performers.iter().map(|p| &p.name).unique().count();
        if players <= 5 || self.performers.iter().any(|p| self.is_group(p)) {
            return None;
        }
        let album_artist = self.album_artist.as_deref()?;
        let performers = match album_artist.rsplit_once(';') {
            Some((_, performers)) => performers.trim(),
            None if self
                .composers
                .iter()
                .any(|c| names_composer(album_artist, c)) =>
            {
                return None;
            }
            None => album_artist.trim(),
        };
        (!performers.is_empty() && performers != "Various Artists").then_some(performers)
    }

    // A single line with the conductors and performers, skipping the
    // names that are already displayed as the artist; the performers are
    // the featured ones, or the ones in the album artist in place of a
    // long list of players
    pub fn summary(&self, artist: Option<&str>) -> String {
        let ensemble = self.ensemble_in_album_artist();
        let conductors = self
            .conductors
            .iter()
            .filter(|c| {
                Some(c.as_str()) != artist && !ensemble.is_some_and(|e| e.contains(c.as_str()))
            })
            .join(", ");
        let performers = match ensemble {
            Some(ensemble) if Some(ensemble) == artist => String::new(),
            Some(ensemble) => ensemble.to_string(),
            None => self
                .featured_performers()
                .into_iter()
                .map(|p| p.name.as_str())
                .filter(|name| Some(*name) != artist && !self.conductors.iter().any(|c| c == name))
                .unique()
                .join(", "),
        };

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
pub fn tag_search_text(tag: &Tag) -> String {
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

    text
}

fn is_link(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    ["http://", "https://", "www."]
        .iter()
        .any(|prefix| word.starts_with(prefix))
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

// The Vorbis comments, APE items, ID3v2 frames and MP4 atoms that lofty
// leaves out of the tags of the `TaggedFile`, as it has no `ItemKey` for
// them
#[derive(Debug, Default)]
pub struct ExtraComments {
    // The JSON written by the "Work Hierarchy" plugin of MusicBrainz Picard
    work_hierarchy: Option<String>,
    // A CUE sheet that splits the file into tracks
    pub cue_sheet: Option<String>,
}

// The name of the tag of the "Work Hierarchy" plugin; MusicBrainz Picard
// writes the tags it has no standard field for as a Vorbis comment of that
// name in capitals, a "TXXX" frame described by it in MP3 files, and a
// "----:com.apple.iTunes:" atom named after it in MP4 files
const WORK_HIERARCHY: &str = "work_hierarchy";

impl ExtraComments {
    fn from_vorbis(comments: Option<&VorbisComments>) -> Self {
        let get = |key| comments.and_then(|c| c.get(key)).map(ToString::to_string);
        ExtraComments {
            work_hierarchy: get("WORK_HIERARCHY"),
            cue_sheet: get("CUESHEET"),
        }
    }

    // Monkey's Audio and WavPack files have their CUE sheet in an APE tag
    fn from_ape(tag: Option<&ApeTag>) -> Self {
        let cue_sheet =
            tag.and_then(|tag| tag.get("Cuesheet"))
                .and_then(|item| match item.value() {
                    ItemValue::Text(text) => Some(text.clone()),
                    _ => None,
                });
        ExtraComments {
            cue_sheet,
            ..Default::default()
        }
    }

    fn from_id3v2(tag: Option<&Id3v2Tag>) -> Self {
        ExtraComments {
            work_hierarchy: tag
                .and_then(|tag| tag.get_user_text(WORK_HIERARCHY))
                .map(ToString::to_string),
            ..Default::default()
        }
    }

    fn from_mp4(ilst: Option<&Ilst>) -> Self {
        let ident = AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: WORK_HIERARCHY.into(),
        };
        let work_hierarchy = ilst.and_then(|ilst| ilst.get(&ident)).and_then(|atom| {
            atom.data().find_map(|data| match data {
                AtomData::UTF8(text) => Some(text.clone()),
                _ => None,
            })
        });
        ExtraComments {
            work_hierarchy,
            ..Default::default()
        }
    }
}

// Reads a file, and the items in `ExtraComments` if it has them
pub fn read_tagged_file(
    probe: Probe<BufReader<File>>,
    options: ParseOptions,
) -> Result<(TaggedFile, ExtraComments), FileParseError> {
    fn read<F: AudioFile + Into<TaggedFile>>(
        probe: Probe<BufReader<File>>,
        options: ParseOptions,
        extra: fn(&F) -> ExtraComments,
    ) -> Result<(TaggedFile, ExtraComments), FileParseError> {
        let file = F::read_from(&mut probe.into_inner(), options)?;
        let extra = extra(&file);
        Ok((file.into(), extra))
    }

    match probe.file_type() {
        Some(FileType::Opus) => read(probe, options, |f: &OpusFile| {
            ExtraComments::from_vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Vorbis) => read(probe, options, |f: &VorbisFile| {
            ExtraComments::from_vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Speex) => read(probe, options, |f: &SpeexFile| {
            ExtraComments::from_vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Flac) => read(probe, options, |f: &FlacFile| {
            ExtraComments::from_vorbis(f.vorbis_comments())
        }),
        Some(FileType::Ape) => read(probe, options, |f: &ApeFile| {
            ExtraComments::from_ape(f.ape())
        }),
        Some(FileType::WavPack) => read(probe, options, |f: &WavPackFile| {
            ExtraComments::from_ape(f.ape())
        }),
        Some(FileType::Mpeg) => read(probe, options, |f: &MpegFile| {
            ExtraComments::from_id3v2(f.id3v2())
        }),
        Some(FileType::Mp4) => read(probe, options, |f: &Mp4File| {
            ExtraComments::from_mp4(f.ilst())
        }),
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
    // The number the plugin gives it: its position in the parent work, or
    // else the number its title starts with
    number: Option<u32>,
    // The number the plugin removed from the title to give the name, as the
    // title writes it, e.g. "V"
    numeral: Option<&'a str>,
    // The names of its composers; `None` if the plugin didn't record them
    composers: Option<Vec<&'a str>>,
    // For the work the track performs, when the track has only part of it,
    // the name the recording's title gives that part, e.g. "Ia. Overture –
    // Adagio" for the Ouverture of the Royal Fireworks, which MusicBrainz
    // links all the tracks of the overture to
    excerpt: Option<&'a str>,
}

impl PathWork<'_> {
    // The name to show, the part's if the track has only part of the work
    fn shown_name(&self) -> &str {
        self.excerpt.unwrap_or(self.name)
    }

    // Whether the name to show has a number of its own, which is the one
    // people know: "no. 22. Chorus" is the first number of Part II of
    // Messiah. A recording's title can number a part without a dot, as in
    // "3 Sarabande"
    fn has_own_number(&self) -> bool {
        let name = self.shown_name();
        is_numbered(name)
            || (self.excerpt.is_some() && name.starts_with(|c: char| c.is_ascii_digit()))
    }

    // The name to show with the number in front, unless it has its own
    fn numbered_name(&self) -> String {
        match self.number {
            Some(n) if !self.has_own_number() => format!("{n}. {}", self.shown_name()),
            _ => self.shown_name().to_string(),
        }
    }

    // The name of a level above the movement, with the number its title
    // gives it, e.g. "III. Sequentia"
    fn level_name(&self) -> String {
        match self.numeral {
            Some(numeral) => format!("{numeral}. {}", self.name),
            None => self.name.to_string(),
        }
    }
}

// The work and the movement to display, from the "WORK_HIERARCHY" tag
#[derive(Debug, PartialEq)]
struct WorkLabels {
    // The name of the work, e.g. "Prelude and Fugue no. 1 in C major, BWV
    // 846"
    work: String,
    // Its full title, with the works it's part of, e.g. "The Well-Tempered
    // Clavier, Book I: Prelude and Fugue no. 1 in C major, BWV 846"
    title: String,
    // None if the track performs the whole work
    movement: Option<MovementName>,
    // The composers of the work, or of the nearest work above it with
    // some; empty if none has any
    composers: Vec<String>,
}

// The work and the movement to display, from the JSON written by the
// "Work Hierarchy" plugin of MusicBrainz Picard. The plugin picks the work
// in each path (`work_index`): the top-level one, or a part of it with a
// catalogue number of its own, like each Prelude and Fugue of The
// Well-Tempered Clavier. The movement names the levels below it. Returns
// `None` if the tags should be used instead: if the track performs parts of
// different works, or a top-level work without parts, which may still be a
// movement in the tags if MusicBrainz didn't link it to its parent work, or
// if an older version of the plugin wrote the tag, without the work
fn work_hierarchy_labels(json: &str) -> Option<WorkLabels> {
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

    let paths = hierarchy["segments"]
        .as_array()?
        .iter()
        .map(|segment| {
            let mut path = segment["path"]
                .as_array()?
                .iter()
                .map(|work| {
                    Some(PathWork {
                        id: work["id"].as_str()?,
                        title: work["title"].as_str()?,
                        name: work["name"].as_str()?,
                        number: work["number"].as_u64().and_then(|n| u32::try_from(n).ok()),
                        numeral: work["numeral"].as_str(),
                        composers: work["composers"].as_array().map(|composers| {
                            composers
                                .iter()
                                .filter_map(|composer| composer["name"].as_str())
                                .collect()
                        }),
                        excerpt: None,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            path.last_mut()?.excerpt = segment["excerpt"].as_str();
            let work_index = usize::try_from(segment["work_index"].as_u64()?).ok()?;
            Some((path, work_index))
        })
        .collect::<Option<Vec<_>>>()?;

    let (first, depth) = paths.first()?;
    let work = first.get(*depth)?;
    // Parts of different works, e.g. the same movement in two catalogues
    if paths
        .iter()
        .any(|(path, index)| index != depth || path.get(*depth).is_none_or(|w| w.id != work.id))
    {
        return None;
    }

    let parts = paths
        .iter()
        .map(|(path, _)| &path[depth + 1..])
        .filter(|parts| !parts.is_empty())
        .collect::<Vec<_>>();
    // A track of a whole part of a work, e.g. a Prelude and Fugue in one
    // track, has no movement; a top-level work without parts is left to the
    // tags
    if parts.is_empty() && *depth == 0 {
        return None;
    }
    // MusicBrainz often links the composer to a work but not to its parts
    let composers = first[..=*depth]
        .iter()
        .rev()
        .filter_map(|work| work.composers.as_ref())
        .find(|composers| !composers.is_empty())
        .map(|composers| composers.iter().map(ToString::to_string).collect())
        .unwrap_or_default();
    Some(WorkLabels {
        work: work.name.to_string(),
        title: work.title.to_string(),
        movement: (!parts.is_empty()).then(|| movement_name(&parts)),
        composers,
    })
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
        let names = works.iter().map(|w| w.shown_name()).join(" / ");
        let numbers = works.iter().map(|w| w.number).collect::<Option<Vec<_>>>();
        match numbers {
            Some(n) if !works.iter().any(|w| w.has_own_number()) => {
                format!("{}. {names}", number_ranges(&n))
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
// Tragödie", "LXVII. Diffusa est", "1a. Introduction", "12a Recitativo",
// "IIIa. [Allegro]", "16: Minuet" or "b. Benedictus"
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
    // A number can have a letter for a part of it, e.g. "12a" or "IIIa"
    let number = token
        .strip_suffix(|c: char| c.is_ascii_lowercase())
        .unwrap_or(token);
    let part = number.len() < token.len();
    if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
        // Without a dot only with a letter, as "2 Waltzes" is a count
        return after.starts_with(['.', ')', ':']) || (part && after.starts_with(' '));
    }
    let roman = !number.is_empty() && number.chars().all(|c| "IVXLC".contains(c));
    let letter = token.len() == 1 && token.chars().all(|c| c.is_ascii_lowercase());
    // A range of parts, e.g. "IIa-b." for the bars of a movement
    let after = match after.strip_prefix(['-', '–']) {
        Some(rest) if part && roman => rest.trim_start_matches(|c: char| c.is_ascii_lowercase()),
        _ => after,
    };
    (roman || letter) && after.starts_with(['.', ')'])
}

// The numbers as a list where consecutive ones are joined into a range,
// e.g. "5–6" or "1–3, 5"
fn number_ranges(numbers: &[u32]) -> String {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &n in numbers {
        match ranges.last_mut() {
            Some((_, end)) if n == *end + 1 => *end = n,
            _ => ranges.push((n, n)),
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

// The artists of a track, if the tags name any
fn track_artist(tag: &Tag) -> Option<String> {
    let artists = tag.get_strings(ItemKey::TrackArtist).join(", ");
    (!artists.is_empty()).then(|| plain_commas(&artists))
}

// The title in a tag, unless it's blank
pub fn track_title(tag: &Tag) -> Option<String> {
    tag.title()
        .filter(|title| !title.trim().is_empty())
        .map(|title| title.to_string())
}

// The album in a tag, unless it's blank
fn track_album(tag: &Tag) -> Option<String> {
    tag.album()
        .filter(|album| !album.trim().is_empty())
        .map(|album| album.to_string())
}

// The album of a track without one in its tags: the folder it's in, as in
// the album grid
fn fallback_album(path: &Path) -> String {
    folder_album_title(path).unwrap_or_else(|| i18n("Unknown album"))
}

// The title of a track without one in its tags, from the name of its file,
// e.g. "Intro" for "01 - Intro.flac". Only a leading track number is left
// out: "Song - Live" could just as well be "Artist - Song", so the rest is
// kept as it is. This can read the folder of the file, so it must not be
// called on the main thread
pub fn title_from_file_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    title_from_name(&stem, |width| has_padded_numbers(path, width))
}

// `padded(width)` tells whether the other names in the folder start with
// numbers padded with zeros to `width` digits
fn title_from_name(stem: &str, padded: impl FnOnce(usize) -> bool) -> String {
    let name = stem.replace('_', " ");
    let name = name.trim();
    without_track_number(name, padded)
        .unwrap_or(name)
        .to_string()
}

// The rest of a name that starts with a track number, e.g. "01 - ", "1. ",
// "3) ", "01 ", or "1-03 " for a disc and a track; the numbers that can be
// part of a title, like in "99 Luftballons" or "3.14 Pi", are kept
fn without_track_number(name: &str, padded: impl FnOnce(usize) -> bool) -> Option<&str> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();

    let n = digits(name);
    if !(1..=3).contains(&n) {
        return None;
    }
    let (number, rest) = name.split_at(n);
    // A disc and a track, e.g. "1-03"
    if let Some(track) = rest
        .strip_prefix('-')
        .filter(|track| n <= 2 && digits(track) == 2)
    {
        return after_track_number(&track[2..], || true);
    }
    // A number followed by a space alone is a track number if it starts
    // with a zero, e.g. "01 Intro", or if the other names in the folder
    // do, e.g. "10 Song" next to "09 Song"
    after_track_number(rest, || (n >= 2 && number.starts_with('0')) || padded(n))
}

// The rest of a name after the separator that follows its track number:
// a dot, a dash or a parenthesis, or if `bare_space()`, a space
fn after_track_number(rest: &str, bare_space: impl FnOnce() -> bool) -> Option<&str> {
    let trimmed = rest.trim_start();
    let after = match trimmed.strip_prefix(['.', '-', ')']) {
        Some(after) => {
            let spaced = after.trim_start();
            // A digit right after the separator continues the number, e.g.
            // "3.14 Pi" or "2-4-6-8 Motorway"; after a space, it starts the
            // title, e.g. "02 - 12 to 12"
            if spaced.len() == after.len() && after.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            spaced
        }
        None if trimmed.len() < rest.len() && bare_space() => trimmed,
        None => return None,
    };
    (!after.is_empty()).then_some(after)
}

// Whether another name in the folder of `path` starts with a number padded
// with zeros to `width` digits, e.g. "09 Song"
fn has_padded_numbers(path: &Path, width: usize) -> bool {
    let Some(entries) = path.parent().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        name.len() > width
            && name[0] == b'0'
            && name[..width].iter().all(u8::is_ascii_digit)
            && !name[width].is_ascii_digit()
    })
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
    // The title shown when the tags have none
    fallback_title: String,
    album: Option<String>,
    // The album shown when the tags have none
    fallback_album: String,
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

    // The title from the tags, or else the one from the file name
    pub fn shown_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.fallback_title)
    }

    pub fn album(&self) -> Option<&str> {
        self.album.as_deref()
    }

    // The album from the tags, or else the name of the track's folder
    pub fn shown_album(&self) -> &str {
        self.album.as_deref().unwrap_or(&self.fallback_album)
    }

    // The album shown in place of the one the tags lack
    pub fn fallback_album(&self) -> Option<&str> {
        self.album.is_none().then_some(self.fallback_album.as_str())
    }

    pub fn tag_text(&self) -> &str {
        &self.tag_text
    }

    pub fn credits(&self) -> &TrackCredits {
        &self.credits
    }

    // The title and the artist, as Copy puts them on the clipboard, e.g.
    // "Summer of '69 — Bryan Adams"; for classical music, the composers
    // and then the performers, as MusicBrainz credits classical releases,
    // e.g. "Zefiro torna — Claudio Monteverdi; Núria Rial". The title
    // comes first, as the performers can be a long list. Nothing is
    // translated, so they can be pasted into a search or a message as
    // they are
    pub fn copied_details(&self) -> String {
        let title = self.shown_title();
        let artist = self.artist();
        // Classical releases are often tagged with the composer as the
        // artist; the performers then come after the composer
        let (composers, artist) = match self.credits.performers_of_composer(artist) {
            Some(performers) => (artist.map(ToString::to_string), Some(performers)),
            None => (
                self.credits.listed_composers(artist),
                artist.map(ToString::to_string),
            ),
        };

        let names = [composers, artist].into_iter().flatten().join("; ");
        if names.is_empty() {
            title.to_string()
        } else {
            format!("{title} — {names}")
        }
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
            title = track_title(tag);
            album = track_album(tag);
            credits = TrackCredits::from_tag(tag, extra);
            replay_gain = ReplayGain::from_tag(tag);
            tag_text = tag_search_text(tag);
            cover = Some(CoverCache::cover_art(path, tag));
        } else {
            warn!("Unable to load primary tag for: {path:?}");
            for tag in tagged_file.tags() {
                debug!("Found tag: {:?}", tag.tag_type());
                // The tags that come later fill in for the ones missing
                // from the first
                artist = artist.or_else(|| track_artist(tag));
                title = title.or_else(|| track_title(tag));
                album = album.or_else(|| track_album(tag));
                credits = TrackCredits::from_tag(tag, extra);
                replay_gain = ReplayGain::from_tag(tag);
                tag_text = tag_search_text(tag);
                // The first tag with a cover has the one we keep
                if cover.as_ref().is_none_or(|c| c.art().is_none()) {
                    cover = Some(CoverCache::cover_art(path, tag));
                }

                if artist.is_some() && title.is_some() {
                    break;
                }
            }
        }
        // A file without tags can still have a cover file next to it
        if cover.is_none() {
            let tag = Tag::new(tagged_file.primary_tag_type());
            cover = Some(CoverCache::cover_art(path, &tag));
        }

        let uuid = read
            .display_name
            .as_deref()
            .map(|name| track_uuid(name, None, [&artist, &title, &album]));
        let fallback_title = if title.is_none() {
            title_from_file_name(path)
        } else {
            String::new()
        };
        // The folder outside the sandbox, as a file that came through the
        // document portal is in a folder named after its document
        let fallback_album = if album.is_none() {
            fallback_album(read.host_path.as_deref().unwrap_or(path))
        } else {
            String::new()
        };

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
            fallback_title,
            album,
            fallback_album,
        }
    }

    // A track of a CUE sheet, with the tags from the sheet over those of
    // its audio file
    fn cue_track(sheet: &SplitSheet, track: &CueTrack, cue_file: &CueFile) -> Self {
        let tag = cue_sheet::track_tag(&cue_file.tag, &sheet.sheet, track);
        // The other items are about the whole file
        let extra = ExtraComments::default();
        let artist = track_artist(&tag);
        let title = track_title(&tag);
        let album = track_album(&tag);

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
            tag_text: tag_search_text(&tag),
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
            // Translators: the title of a track of a CUE sheet that has
            // none; `{}` is its number
            fallback_title: i18n_f("Track {}", &[&track.number.to_string()]),
            // The album is in the folder of the sheet
            fallback_album: if album.is_none() {
                fallback_album(&sheet.path)
            } else {
                String::new()
            },
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
    utils::hex(&hasher.finalize())
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
            fallback_title: String::new(),
            album: Some("Invalid Album".to_string()),
            fallback_album: String::new(),
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
        // The conductors and performers, as a single line; if the tags
        // have none and the artist is the composer, the performers in the
        // album artist
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
            self.data.borrow().shown_title().to_string()
        }

        fn album(&self) -> String {
            self.data.borrow().shown_album().to_string()
        }

        fn credits(&self) -> String {
            let data = self.data.borrow();
            let credits = data.credits();
            let summary = credits.summary(data.artist());
            if summary.is_empty() {
                credits
                    .performers_of_composer(data.artist())
                    .unwrap_or_default()
            } else {
                summary
            }
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

    // The title, artist and album in the tags, without the ones shown in
    // their place
    pub fn tag_title(&self) -> Option<String> {
        self.imp().data.borrow().title().map(ToString::to_string)
    }

    pub fn tag_artist(&self) -> Option<String> {
        self.imp().data.borrow().artist().map(ToString::to_string)
    }

    pub fn tag_album(&self) -> Option<String> {
        self.imp().data.borrow().album().map(ToString::to_string)
    }

    pub fn credit_details(&self) -> TrackCredits {
        self.imp().data.borrow().credits().clone()
    }

    pub fn copied_details(&self) -> String {
        self.imp().data.borrow().copied_details()
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

    // The composers of the work a movement is part of, which all its
    // movements share, or `composer()` if the tags don't say
    pub fn work_composers(&self) -> String {
        let composers = self.imp().data.borrow().credits().work_composers.join(", ");
        if composers.is_empty() {
            self.composer()
        } else {
            composers
        }
    }

    // The release the track is from: its MusicBrainz id, or else its album
    // and album artist
    pub fn release_key(&self) -> String {
        let data = self.imp().data.borrow();
        let credits = data.credits();
        match &credits.musicbrainz.release {
            Some(id) => id.clone(),
            None => format!(
                "{}\0{}",
                data.shown_album(),
                credits.album_artist.as_deref().unwrap_or_default()
            ),
        }
    }

    // Whether the track comes after another on their album, by disc and
    // track number, or else whether it's another track
    pub fn follows_on_album(&self, other: &Track) -> bool {
        let position = |track: &Track| {
            let data = track.imp().data.borrow();
            let credits = data.credits();
            Some((credits.disc_number.unwrap_or(1), credits.track_number?))
        };
        match (position(other), position(self)) {
            (Some(before), Some(after)) => after > before,
            _ => self.uri() != other.uri(),
        }
    }

    // The artist that shuffling keeps from coming twice in a row: the
    // composers of a movement, as its work shows them, and otherwise the
    // artist in the tags
    pub fn shuffle_artist(&self) -> Option<String> {
        let data = self.imp().data.borrow();
        let composers = &data.credits().composers;
        if self.movement_work().is_some() && !composers.is_empty() {
            Some(composers.join(", "))
        } else {
            data.artist().map(ToString::to_string)
        }
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
                let key = [
                    self.artist(),
                    self.title(),
                    data.tag_text().to_string(),
                    data.fallback_album().unwrap_or_default().to_string(),
                ]
                .join(" ");
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
        Credit, ExtraComments, MovementName, MusicBrainzIds, Track, TrackCredits, TrackData,
        WorkLabels, album_title_search_text, is_numbered, normalize_search_text, number_ranges,
        tag_search_text, title_from_name, track_album, track_title, work_hierarchy_composer_ids,
        work_hierarchy_labels,
    };

    #[test]
    fn file_names_lose_only_their_track_numbers() {
        let cases = [
            ("01 - Intro", "Intro"),
            ("01. Intro", "Intro"),
            ("01 Intro", "Intro"),
            ("01-Intro", "Intro"),
            ("03_Song_Name", "Song Name"),
            ("1-03 Intro", "Intro"),
            ("3) Intro", "Intro"),
            ("10 - Intro", "Intro"),
            ("Song - Live", "Song - Live"),
            ("01 - Song - Live", "Song - Live"),
            ("Artist - Song", "Artist - Song"),
            ("Song.Name", "Song.Name"),
            // Titles starting with a number
            ("02 12 to 12", "12 to 12"),
            ("02 - 12 to 12", "12 to 12"),
            // Numbers that can be part of the title
            ("99 Luftballons", "99 Luftballons"),
            ("2001 A Space Odyssey", "2001 A Space Odyssey"),
            ("2-4-6-8 Motorway", "2-4-6-8 Motorway"),
            ("3.14 Pi", "3.14 Pi"),
            // Nothing would be left
            ("01", "01"),
            ("01 - ", "01 -"),
        ];
        for (name, title) in cases {
            assert_eq!(title_from_name(name, |_| false), title, "{name}");
        }

        // Next to names that start with "01" to "09"
        assert_eq!(
            title_from_name("10 under the mat", |width| width == 2),
            "under the mat"
        );
        assert_eq!(
            title_from_name("99 Luftballons", |width| width == 3),
            "99 Luftballons"
        );
    }

    #[test]
    fn blank_titles_are_missing() {
        let mut tag = Tag::new(TagType::VorbisComments);
        assert_eq!(track_title(&tag), None);
        tag.insert_text(ItemKey::TrackTitle, "  ".to_string());
        assert_eq!(track_title(&tag), None);
        tag.insert_text(ItemKey::TrackTitle, "Aqua".to_string());
        assert_eq!(track_title(&tag).as_deref(), Some("Aqua"));

        assert_eq!(track_album(&tag), None);
        tag.insert_text(ItemKey::AlbumTitle, " ".to_string());
        assert_eq!(track_album(&tag), None);
        tag.insert_text(ItemKey::AlbumTitle, "Aquarium".to_string());
        assert_eq!(track_album(&tag).as_deref(), Some("Aquarium"));
    }

    #[test]
    fn search_text_has_no_links() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::TrackTitle, "Aqua".to_string());
        tag.insert_text(
            ItemKey::Comment,
            "https://www.youtube.com/watch?v=LLIsy64Sv5c".to_string(),
        );
        let text = normalize_search_text(&tag_search_text(&tag));
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
    // id, name and number of each work, the work being the one at
    // `work_index` in each path
    fn hierarchy_at(paths: &[&[Work]], work_index: usize) -> String {
        let segments = paths
            .iter()
            .map(|path| {
                let path = path
                    .iter()
                    .map(|(id, name, number)| {
                        serde_json::json!({
                            "id": id, "title": name, "name": name, "number": number,
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({ "path": path, "work_index": work_index, "attributes": [] })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "version": 1, "segments": segments }).to_string()
    }

    fn hierarchy(paths: &[&[Work]]) -> String {
        hierarchy_at(paths, 0)
    }

    fn labels_at(paths: &[&[Work]], work_index: usize) -> Option<(String, String)> {
        work_hierarchy_labels(&hierarchy_at(paths, work_index))
            .map(|l| (l.work, l.movement.map_or_else(String::new, |m| m.full())))
    }

    fn labels(paths: &[&[Work]]) -> Option<(String, String)> {
        labels_at(paths, 0)
    }

    fn movement(paths: &[&[Work]]) -> Option<String> {
        labels(paths).map(|(_, movement)| movement)
    }

    // The movement of a "WORK_HIERARCHY" tag
    fn movement_of(json: &serde_json::Value) -> Option<String> {
        work_hierarchy_labels(&json.to_string())
            .and_then(|l| l.movement)
            .map(|m| m.full())
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
    fn work_hierarchy_numbers_the_levels_as_their_titles_do() {
        let json = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0, "path": [
            { "id": "r", "title": "Requiem, op. 89", "name": "Requiem, op. 89", "number": null },
            { "id": "1", "title": "Requiem, op. 89: I. oddíl", "name": "oddíl",
              "number": 1, "numeral": "I" },
            { "id": "3", "title": "…: III. Sequentia", "name": "Sequentia",
              "number": 3, "numeral": "III" },
            { "id": "q", "title": "…: Quid sum miser", "name": "Quid sum miser", "number": 3 },
        ]}]});
        assert_eq!(
            movement_of(&json).as_deref(),
            Some("I. oddíl: III. Sequentia: 3. Quid sum miser")
        );
    }

    #[test]
    fn work_hierarchy_keeps_the_numbers_of_parts_of_movements() {
        // The third movement of the suite, in three tracks
        let json = serde_json::json!({ "version": 1, "segments": [{ "work_index": 1, "path": [
            { "id": "w", "title": "Water Music, HWV 348–350", "name": "Water Music, HWV 348–350",
              "number": null },
            { "id": "s", "title": "Water Music Suite no. 1 in F major, HWV 348",
              "name": "Water Music Suite no. 1 in F major, HWV 348", "number": 1 },
            { "id": "3", "title": "…: III. Allegro - Andante - Allegro da capo",
              "name": "Allegro - Andante - Allegro da capo", "number": 3, "numeral": "III" },
            { "id": "3a", "title": "…: IIIa. [Allegro]", "name": "IIIa. [Allegro]", "number": 1 },
        ]}]});
        assert_eq!(
            movement_of(&json).as_deref(),
            Some("III. Allegro - Andante - Allegro da capo: IIIa. [Allegro]")
        );
    }

    #[test]
    fn a_part_of_a_movement_is_named_after_its_recording() {
        let movement = |excerpt: Option<&str>| {
            movement_of(&serde_json::json!({ "version": 1, "segments": [{
                "work_index": 0, "attributes": ["partial"], "excerpt": excerpt, "path": [
                    { "id": "hwv351", "title": "Music for the Royal Fireworks, HWV 351",
                      "name": "Music for the Royal Fireworks, HWV 351", "number": null },
                    { "id": "i", "title": "Music for the Royal Fireworks, HWV 351: I. Ouverture",
                      "name": "Ouverture", "number": 1, "numeral": "I" },
                ],
            }]}))
        };
        // An overture in two tracks
        assert_eq!(
            movement(Some("Ia. Overture – Adagio")).as_deref(),
            Some("Ia. Overture – Adagio")
        );
        assert_eq!(
            movement(Some("IIa. Gloria")).as_deref(),
            Some("IIa. Gloria")
        );
        // The number of the movement goes before a part without one
        assert_eq!(movement(Some("Adagio")).as_deref(), Some("1. Adagio"));
        assert_eq!(
            movement(Some("3 Sarabande")).as_deref(),
            Some("3 Sarabande")
        );
        // When the recording's title doesn't name the part
        assert_eq!(movement(None).as_deref(), Some("1. Ouverture"));

        // A number of an opera, in its act
        let json = serde_json::json!({ "version": 1, "segments": [{
            "work_index": 0, "attributes": ["partial"],
            "excerpt": "Air « Prendre le dessin d’un bijou » (Gerald)", "path": [
                { "id": "lakme", "title": "Lakmé", "name": "Lakmé", "number": null },
                { "id": "act1", "title": "Lakmé: Acte I", "name": "Acte I", "number": 1 },
                { "id": "air", "title": "…", "name": "Air « Prendre le dessin d’un bijou »",
                  "number": 8 },
            ],
        }]});
        assert_eq!(
            movement_of(&json).as_deref(),
            Some("Acte I: 8. Air « Prendre le dessin d’un bijou » (Gerald)")
        );
    }

    #[test]
    fn work_hierarchy_numbers_the_lowest_level() {
        let wtc = ("wtc", "The Well-Tempered Clavier", None);
        let book = ("book1", "Book I", Some(1));
        let pf = ("pf12", "Prelude and Fugue no. 12 in F minor", Some(12));
        assert_eq!(
            movement(&[&[wtc, book, pf, ("p", "Prelude", Some(1))]]).as_deref(),
            Some("Book I: Prelude and Fugue no. 12 in F minor: 1. Prelude")
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
        let name = |paths: &[&[Work]]| {
            work_hierarchy_labels(&hierarchy(paths))
                .unwrap()
                .movement
                .unwrap()
        };
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
        // Without a number, there is none
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
        // A tag from a version of the plugin that didn't pick the work
        let json = r#"{"version":1,"segments":[{"path":[
            {"id":"s","title":"Suite","name":"Suite","position":null},
            {"id":"p","title":"Suite: I. Prélude","name":"Prélude","position":1}
        ]}]}"#;
        assert_eq!(work_hierarchy_labels(json), None);
        assert_eq!(work_hierarchy_labels("not json"), None);
        assert_eq!(
            work_hierarchy_labels(r#"{"version":2,"segments":[]}"#),
            None
        );
        assert_eq!(
            work_hierarchy_labels(r#"{"version":1,"segments":[]}"#),
            None
        );
    }

    #[test]
    fn the_work_is_the_one_the_plugin_picks() {
        // A Prelude and Fugue of The Well-Tempered Clavier, which has a
        // catalogue number of its own
        let json = serde_json::json!({ "version": 1, "segments": [{ "work_index": 2, "path": [
            { "id": "wtc", "title": "The Well-Tempered Clavier",
              "name": "The Well-Tempered Clavier", "number": null },
            { "id": "book1", "title": "The Well-Tempered Clavier, Book I",
              "name": "Book I", "number": 1 },
            { "id": "bwv846",
              "title": "The Well-Tempered Clavier, Book I: Prelude and Fugue no. 1 in C major, BWV 846",
              "name": "Prelude and Fugue no. 1 in C major, BWV 846", "number": 1 },
            { "id": "f", "title": "…: Fugue", "name": "Fugue", "number": 2 },
        ]}]});
        assert_eq!(
            work_hierarchy_labels(&json.to_string()),
            Some(WorkLabels {
                work: "Prelude and Fugue no. 1 in C major, BWV 846".to_string(),
                title:
                    "The Well-Tempered Clavier, Book I: Prelude and Fugue no. 1 in C major, BWV 846"
                        .to_string(),
                movement: Some(MovementName {
                    levels: None,
                    movement: "2. Fugue".to_string(),
                }),
                composers: Vec::new(),
            })
        );

        let wtc = ("wtc", "The Well-Tempered Clavier", None);
        let book = ("book1", "Book I", Some(1));
        let pf = (
            "bwv846",
            "Prelude and Fugue no. 1 in C major, BWV 846",
            Some(1),
        );
        let prelude = ("p", "Prelude", Some(1));
        let fugue = ("f", "Fugue", Some(2));
        // Both movements of it
        assert_eq!(
            labels_at(&[&[wtc, book, pf, prelude], &[wtc, book, pf, fugue]], 2),
            Some((
                "Prelude and Fugue no. 1 in C major, BWV 846".to_string(),
                "1–2. Prelude / Fugue".to_string()
            ))
        );
        // A whole Prelude and Fugue in one track is a work, not a movement
        assert_eq!(
            labels_at(&[&[wtc, book, pf]], 2),
            Some((
                "Prelude and Fugue no. 1 in C major, BWV 846".to_string(),
                String::new()
            ))
        );
        // Movements of two of them, which are only parts of the same
        // collection
        let pf2 = (
            "bwv847",
            "Prelude and Fugue no. 2 in C minor, BWV 847",
            Some(2),
        );
        assert_eq!(
            labels(&[&[wtc, book, pf, fugue], &[wtc, book, pf2, prelude]]),
            Some((
                "The Well-Tempered Clavier".to_string(),
                "Book I: Prelude and Fugue no. 1 in C major, BWV 846: 2. Fugue / Prelude and Fugue no. 2 in C minor, BWV 847: 1. Prelude"
                    .to_string()
            ))
        );
    }

    #[test]
    fn work_hierarchy_replaces_the_work_tags() {
        // Whatever the work and movement tags say
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::Work, "Messiah, HWV 56: Part II".to_string());
        tag.insert_text(ItemKey::Movement, "Chorus".to_string());
        tag.insert_text(ItemKey::MovementNumber, "1".to_string());
        let part = ("part2", "Part II", Some(2));
        let chorus = ("22", "no. 22. Chorus", Some(1));
        let extra = ExtraComments {
            work_hierarchy: Some(hierarchy(&[&[MESSIAH, part, chorus]])),
            ..Default::default()
        };
        let credits = TrackCredits::from_tag(&tag, &extra);
        assert_eq!(credits.work.as_deref(), Some("Messiah, HWV 56"));
        assert_eq!(
            credits.movement_label().as_deref(),
            Some("Part II: no. 22. Chorus")
        );
    }

    #[test]
    fn work_hierarchy_is_read_from_mp3_and_mp4_files() {
        use lofty::{
            id3::v2::Id3v2Tag,
            mp4::{Atom, AtomData, AtomIdent, Ilst},
        };

        let json = r#"{"version":1,"segments":[]}"#;
        // As MusicBrainz Picard writes the tags it has no field for
        let mut id3v2 = Id3v2Tag::new();
        let _ = id3v2.insert_user_text("work_hierarchy".to_string(), json.to_string());
        assert_eq!(
            ExtraComments::from_id3v2(Some(&id3v2))
                .work_hierarchy
                .as_deref(),
            Some(json)
        );
        let mut ilst = Ilst::new();
        ilst.insert(Atom::new(
            AtomIdent::Freeform {
                mean: "com.apple.iTunes".into(),
                name: "work_hierarchy".into(),
            },
            AtomData::UTF8(json.to_string()),
        ));
        assert_eq!(
            ExtraComments::from_mp4(Some(&ilst))
                .work_hierarchy
                .as_deref(),
            Some(json)
        );

        assert_eq!(
            ExtraComments::from_id3v2(Some(&Id3v2Tag::new())).work_hierarchy,
            None
        );
        assert_eq!(ExtraComments::from_mp4(None).work_hierarchy, None);
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
            // Parts of a number
            "IIIa. [Allegro]",
            "XXVIIa. Arie (Sopran, Alt) und Chor",
            // No. 50a of the St Matthew Passion
            "La. Recitativo (Evangelista)",
            "12a Recitativo \"Und Hannas sandte ihn gebunden\"",
            "IIa-b. Andante quasi Allegretto [Bars 1-50]",
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
            "2 Waltzes, op. 54",
            "Il Contento",
            "La Balançoire",
            "In der Ferne",
        ] {
            assert!(!is_numbered(name), "{name}");
        }
    }

    #[test]
    fn composers_are_listed_for_classical_music() {
        let brentner = TrackCredits {
            composers: vec!["Jan Josef Ignác Brentner".to_string()],
            genres: vec!["Classical".to_string()],
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
    fn copied_details_name_the_composers_of_classical_music() {
        let song = TrackData {
            title: Some("Help!".to_string()),
            artist: Some("The Beatles".to_string()),
            credits: TrackCredits {
                composers: vec!["John Lennon".to_string(), "Paul McCartney".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(song.copied_details(), "Help! — The Beatles");

        let madrigal = TrackData {
            title: Some("Zefiro torna".to_string()),
            artist: Some("Núria Rial, Oleguer Aymamí".to_string()),
            credits: TrackCredits {
                composers: vec!["Claudio Monteverdi".to_string()],
                genres: vec!["Classical".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            madrigal.copied_details(),
            "Zefiro torna — Claudio Monteverdi; Núria Rial, Oleguer Aymamí"
        );

        // Without an artist, the title from the file name if need be
        let untagged = TrackData {
            title: None,
            fallback_title: "Zefiro torna".to_string(),
            artist: None,
            ..madrigal
        };
        assert_eq!(
            untagged.copied_details(),
            "Zefiro torna — Claudio Monteverdi"
        );

        // With the composer as the artist, the performers come from the
        // album artist, or from the track's tags if it has them
        let requiem = TrackData {
            title: Some("Ein deutsches Requiem, Op. 45: 1. Selig sind".to_string()),
            artist: Some("Johannes Brahms".to_string()),
            credits: TrackCredits {
                album_artist: Some(
                    "Johannes Brahms; Kammerchor Stuttgart & Frieder Bernius".to_string(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            requiem.copied_details(),
            "Ein deutsches Requiem, Op. 45: 1. Selig sind — \
             Johannes Brahms; Kammerchor Stuttgart & Frieder Bernius"
        );
        let requiem = TrackData {
            credits: TrackCredits {
                composers: vec!["Johannes Brahms".to_string()],
                conductors: vec!["Frieder Bernius".to_string()],
                performers: vec![Credit::parse("Kammerchor Stuttgart (choir vocals)")],
                ..requiem.credits
            },
            ..requiem
        };
        assert_eq!(
            requiem.copied_details(),
            "Ein deutsches Requiem, Op. 45: 1. Selig sind — \
             Johannes Brahms; Frieder Bernius · Kammerchor Stuttgart"
        );

        // Not when the artist isn't among the names before the semicolon
        let duet = TrackData {
            title: Some("Song".to_string()),
            artist: Some("Singer".to_string()),
            credits: TrackCredits {
                album_artist: Some("Band; Singer".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(duet.copied_details(), "Song — Singer");
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
    fn work_composers_are_those_of_the_work_not_the_movement() {
        let work = |id: &str, name: &str, composers: serde_json::Value| serde_json::json!({ "id": id, "title": name, "name": name, "composers": composers });
        // The composers of the work at `work_index`
        let composers = |work_index: usize, path: Vec<serde_json::Value>| {
            let json = serde_json::json!({ "version": 1, "segments": [
                { "path": path, "work_index": work_index },
            ] });
            work_hierarchy_labels(&json.to_string()).map(|labels| labels.composers)
        };
        let vivaldi = serde_json::json!([{ "id": "aaaa", "name": "Antonio Vivaldi" }]);

        // MusicBrainz credits Bottesini on two movements of RV 531, which
        // would split the work in the queue
        assert_eq!(
            composers(
                0,
                vec![
                    work(
                        "rv531",
                        "Concerto for Two Cellos in G minor, RV 531",
                        vivaldi
                    ),
                    work(
                        "largo",
                        "Largo",
                        serde_json::json!([
                            { "id": "bbbb", "name": "Giovanni Bottesini" },
                            { "id": "aaaa", "name": "Antonio Vivaldi" },
                        ]),
                    ),
                ]
            ),
            Some(vec!["Antonio Vivaldi".to_string()])
        );

        // A part of a collection that is the work has its own, or else
        // those of the nearest work above it with some
        let bach = serde_json::json!([{ "id": "cccc", "name": "Johann Sebastian Bach" }]);
        let wtc = |pf_composers| {
            composers(
                2,
                vec![
                    work("wtc", "The Well-Tempered Clavier", bach.clone()),
                    work("book1", "Book I", serde_json::json!([])),
                    work(
                        "bwv846",
                        "Prelude and Fugue no. 1 in C major, BWV 846",
                        pf_composers,
                    ),
                    work("f", "Fugue", serde_json::Value::Null),
                ],
            )
        };
        let anon = serde_json::json!([{ "id": "dddd", "name": "Anonymous" }]);
        assert_eq!(wtc(anon), Some(vec!["Anonymous".to_string()]));
        assert_eq!(
            wtc(serde_json::Value::Null),
            Some(vec!["Johann Sebastian Bach".to_string()])
        );

        // And none if no work has any
        assert_eq!(
            composers(
                0,
                vec![
                    work("fvb", "Fitzwilliam Virginal Book", serde_json::json!([])),
                    work("walsingham", "Walsingham", serde_json::Value::Null),
                ]
            ),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_movement_continues_its_work_in_the_order_of_its_album() {
        use crate::work_sections::continues_work;

        let movement = |release: &str, track: Option<u32>, composers: &[&str], file: &str| {
            Track::new(TrackData {
                credits: TrackCredits {
                    work: Some("Concerto for Two Cellos in G minor, RV 531".to_string()),
                    movement: Some("Allegro".to_string()),
                    composers: composers.iter().map(ToString::to_string).collect(),
                    work_composers: vec!["Antonio Vivaldi".to_string()],
                    track_number: track,
                    musicbrainz: MusicBrainzIds {
                        release: Some(release.to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                file: gtk::gio::File::for_path(file),
                ..Default::default()
            })
        };
        let vivaldi = ["Antonio Vivaldi"];
        let bottesini = ["Giovanni Bottesini", "Antonio Vivaldi"];
        let x2 = [
            movement("x2", Some(4), &vivaldi, "/x2/04.opus"),
            movement("x2", Some(5), &bottesini, "/x2/05.opus"),
            movement("x2", Some(6), &bottesini, "/x2/06.opus"),
        ];

        // Whatever else its movements credit
        assert!(continues_work(&x2[0], &x2[1]));
        assert!(continues_work(&x2[1], &x2[2]));
        // Not another recording
        let bizarri = movement("bizarri", Some(11), &vivaldi, "/bizarri/11.opus");
        assert!(!continues_work(&x2[2], &bizarri));
        // Nor the work added again
        assert!(!continues_work(&x2[2], &x2[0]));
        assert!(!continues_work(&x2[1], &x2[1]));

        // Without track numbers, only the same track again
        let a = movement("x2", None, &vivaldi, "/x2/a.opus");
        let b = movement("x2", None, &vivaldi, "/x2/b.opus");
        assert!(continues_work(&a, &b));
        assert!(!continues_work(&a, &a));
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

        // Without performers in the tags, they come from the album artist,
        // when the artist is one of the composers it lists first
        let requiem = TrackCredits {
            album_artist: Some(
                "Johannes Brahms; Kammerchor Stuttgart & Frieder Bernius".to_string(),
            ),
            ..Default::default()
        };
        assert_eq!(
            requiem
                .performers_of_composer(Some("Johannes Brahms"))
                .as_deref(),
            Some("Kammerchor Stuttgart & Frieder Bernius")
        );
        let tagged_requiem = TrackCredits {
            composers: vec!["Johannes Brahms".to_string()],
            ..requiem.clone()
        };
        assert_eq!(
            tagged_requiem
                .performers_of_composer(Some("Johannes Brahms"))
                .as_deref(),
            Some("Kammerchor Stuttgart & Frieder Bernius")
        );
        assert_eq!(
            requiem.performers_of_composer(Some("Frieder Bernius")),
            None
        );

        // The composers can be separated by semicolons too
        let vespers = TrackCredits {
            album_artist: Some(
                "František Ignác Tůma; Jan Dismas Zelenka; Collegium 1704, Václav Luks".to_string(),
            ),
            ..Default::default()
        };
        for composer in ["František Ignác Tůma", "Jan Dismas Zelenka"] {
            assert_eq!(
                vespers.performers_of_composer(Some(composer)).as_deref(),
                Some("Collegium 1704, Václav Luks")
            );
        }

        // Not when the composer performs too, as their name would be shown
        // twice
        let savall = TrackCredits {
            album_artist: Some("Jordi Savall; Jordi Savall, Hespèrion XXI".to_string()),
            ..Default::default()
        };
        assert_eq!(savall.performers_of_composer(Some("Jordi Savall")), None);
    }

    #[test]
    fn groups_are_named_in_place_of_their_players() {
        let credits = |album_artist: &str, conductors: &[&str], performers: &[&str]| TrackCredits {
            album_artist: Some(album_artist.to_string()),
            conductors: conductors.iter().map(ToString::to_string).collect(),
            performers: performers.iter().map(|p| Credit::parse(p)).collect(),
            ..Default::default()
        };

        // The orchestra stands for its players, with the soloist, who
        // conducts too
        let concerto = credits(
            "Antonio Vivaldi; La Serenissima, Adrian Chandler",
            &["Adrian Chandler"],
            &[
                "Robin Bigwood (harpsichord)",
                "Robin Bigwood (organ)",
                "Lynda Sayce (theorbo)",
                "Abel Balazs (violin)",
                "Ellen Bundy (violin)",
                "Carina Drury (cello)",
                "Adrian Chandler (solo violin)",
                "Sam Kennedy (viola)",
                "Jan Zahourek (double bass)",
                "La Serenissima (orchestra)",
            ],
        );
        assert_eq!(
            concerto.summary(Some("Antonio Vivaldi")),
            "Adrian Chandler · La Serenissima"
        );

        // The soloists of a double concerto, before the orchestra
        let horns = credits(
            "Antonio Vivaldi; Adrian Chandler, La Serenissima",
            &["Adrian Chandler"],
            &[
                "La Serenissima (orchestra)",
                "Abel Balazs (violin)",
                "Jocelyn Lightfoot (solo horn)",
                "Anneke Scott (solo horn)",
            ],
        );
        assert_eq!(
            horns.summary(Some("Antonio Vivaldi")),
            "Adrian Chandler · Jocelyn Lightfoot, Anneke Scott, La Serenissima"
        );

        // A soloist that isn't marked solo is in the album artist
        let cello = credits(
            "Antonín Dvořák; Czech Philharmonic, Jiří Bělohlávek, Alisa Weilerstein",
            &["Jiří Bělohlávek"],
            &["Česká filharmonie (orchestra)", "Alisa Weilerstein (cello)"],
        );
        assert_eq!(
            cello.summary(Some("Antonín Dvořák")),
            "Jiří Bělohlávek · Alisa Weilerstein, Česká filharmonie"
        );

        // A quartet stands for its members, but not for its guests
        let quartet = credits(
            "Franz Schubert; Pavel Haas Quartet, Danjulo Ishizaka",
            &[],
            &[
                "Pavel Haas Quartet (string quartet)",
                "Peter Jarůšek (cello)",
                "Veronika Jarůšková (violin)",
                "Marek Zwiebel (violin)",
                "Pavel Nikl (viola)",
            ],
        );
        assert_eq!(
            quartet.summary(Some("Franz Schubert")),
            "Pavel Haas Quartet"
        );
        let sextet = credits(
            "Johannes Brahms; Belcea Quartet, Tabea Zimmermann, Jean‐Guihen Queyras",
            &[],
            &[
                "Belcea Quartet (string quartet)",
                "Corina Belcea (violin)",
                "Axel Schacher (violin)",
                "Krzysztof Chorzelski (viola)",
                "Tabea Zimmermann (viola)",
                "Antoine Lederlin (cello)",
                "Jean‐Guihen Queyras (cello)",
            ],
        );
        assert_eq!(
            sextet.summary(Some("Johannes Brahms")),
            "Tabea Zimmermann, Jean‐Guihen Queyras, Belcea Quartet"
        );

        // Up to four singers are soloists
        let stabat_mater = credits(
            "Antonín Dvořák; Collegium Vocale Gent, Antwerp Symphony Orchestra, Philippe Herreweghe",
            &["Philippe Herreweghe"],
            &[
                "Antwerp Symphony Orchestra (orchestra)",
                "Florian Boesch (bass vocals)",
                "Collegium Vocale Gent (choir vocals)",
                "Ilse Eerens (soprano vocals)",
                "Maximilian Schmitt (tenor vocals)",
                "Michaela Selinger (mezzo-soprano vocals)",
            ],
        );
        assert_eq!(
            stabat_mater.summary(Some("Antonín Dvořák")),
            "Philippe Herreweghe · Florian Boesch, Ilse Eerens, Maximilian Schmitt, \
             Michaela Selinger, Antwerp Symphony Orchestra, Collegium Vocale Gent"
        );
        // More are the members of a vocal ensemble
        let motet = credits(
            "Giovanni Pierluigi da Palestrina; Stile Antico",
            &[],
            &[
                "James Arthur (bass vocals)",
                "Nathan Harrison (bass vocals)",
                "Emma Ashby (alto vocals)",
                "Cara Curran (alto vocals)",
                "Helen Ashby (soprano vocals)",
                "Andrew Griffiths (tenor vocals)",
                "Stile Antico (choir vocals)",
            ],
        );
        assert_eq!(
            motet.summary(Some("Giovanni Pierluigi da Palestrina")),
            "Stile Antico"
        );

        // "instruments" is a group's when the album artist names it
        let sonata = credits(
            "Arcangelo Corelli; The Purcell Quartet",
            &[],
            &[
                "Richard Boothby (cello)",
                "Catherine Mackintosh (violin)",
                "Elizabeth Wallfisch (violin)",
                "The Purcell Quartet (instruments)",
                "Robert Woolley (organ)",
            ],
        );
        assert_eq!(
            sonata.summary(Some("Arcangelo Corelli")),
            "The Purcell Quartet"
        );

        // Without a group, the album artist names the performers of a long
        // list, and its conductor isn't named twice
        let players = [
            "Elin Fleischer (concertmaster)",
            "Jörgen Ådvall (tuba)",
            "Simon Fröh (tuba)",
            "Bo Åhgren (wind instruments)",
            "Per Fastrup (wind instruments)",
            "Carl‐Johan Andersson (instruments)",
            "Ebba Eriksson (clarinet)",
        ];
        let march = credits(
            "Carl Teike; The Royal Swedish Navy Band, Alexander Hanson",
            &["Alexander Hanson"],
            &players,
        );
        assert_eq!(
            march.summary(Some("Carl Teike")),
            "The Royal Swedish Navy Band, Alexander Hanson"
        );
        // Or the whole album artist, if it has no composer
        let romanesca = credits("Sol Gabetta", &["Dmitry Smirnov"], &players);
        assert_eq!(
            romanesca.summary(Some("[anonymous]")),
            "Dmitry Smirnov · Sol Gabetta"
        );
        // But not if it names the composer, or nobody
        let various = credits("Various Artists", &[], &players[..6]);
        assert_eq!(
            various.summary(None),
            "Elin Fleischer, Jörgen Ådvall, Simon Fröh, Bo Åhgren, Per Fastrup, \
             Carl‐Johan Andersson"
        );
        let zefiro = TrackCredits {
            composers: vec!["Ludwig van Beethoven".to_string()],
            ..credits("Ludwig van Beethoven, Ensemble Zefiro", &[], &players[..6])
        };
        assert_eq!(zefiro.summary(None), various.summary(None));
        // The players of a quintet still fit
        let quintet = credits("Anton Eberl; Consortium Classicum", &[], &players[..5]);
        assert_eq!(
            quintet.summary(Some("Anton Eberl")),
            "Elin Fleischer, Jörgen Ådvall, Simon Fröh, Bo Åhgren, Per Fastrup"
        );

        // Copy names the same performers
        let allegro = TrackData {
            title: Some(
                "Concerto VII for Violin, Strings and Continuo in D minor, RV 242: Allegro"
                    .to_string(),
            ),
            artist: Some("Antonio Vivaldi".to_string()),
            credits: TrackCredits {
                composers: vec!["Antonio Vivaldi".to_string()],
                ..concerto
            },
            ..Default::default()
        };
        assert_eq!(
            allegro.copied_details(),
            "Concerto VII for Violin, Strings and Continuo in D minor, RV 242: Allegro — \
             Antonio Vivaldi; Adrian Chandler · La Serenissima"
        );
    }

    #[test]
    fn number_ranges_join_consecutive_numbers() {
        assert_eq!(number_ranges(&[5]), "5");
        assert_eq!(number_ranges(&[5, 6]), "5–6");
        assert_eq!(number_ranges(&[1, 2, 3, 5]), "1–3, 5");
        assert_eq!(number_ranges(&[6, 5]), "6, 5");
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

    #[test]
    fn tracks_without_titles_show_their_file_name_or_number() {
        let name = format!("dismas-untitled-{}", std::process::id());
        let dir = std::env::temp_dir().join(&name);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("01 - Intro.wav");
        crate::utils::write_test_wav(&wav, 4);

        // Nor an album, which is the folder's
        let whole = read_tracks(&[&wav]).remove(0);
        assert_eq!(whole[0].title(), None);
        assert_eq!(whole[0].shown_title(), "Intro");
        assert_eq!(whole[0].album(), None);
        assert_eq!(whole[0].shown_album(), name);
        assert_eq!(whole[0].fallback_album(), Some(name.as_str()));

        // The other names in the folder start with "01" to "09"
        let tenth = dir.join("10 under the mat.wav");
        crate::utils::write_test_wav(&tenth, 1);
        let tenth = read_tracks(&[&tenth]).remove(0);
        assert_eq!(tenth[0].shown_title(), "under the mat");

        let cue = dir.join("Album.cue");
        std::fs::write(
            &cue,
            "FILE \"01 - Intro.wav\" WAVE\n\
             TRACK 01 AUDIO\nTITLE \"One\"\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 00:02:00\n",
        )
        .unwrap();
        let tracks = read_tracks(&[&cue]).remove(0);
        let titles: Vec<_> = tracks.iter().map(super::TrackData::shown_title).collect();
        assert_eq!(titles, vec!["One", "Track 2"]);
        assert_eq!(tracks[0].shown_album(), name);

        // With an album, there's nothing in its place
        std::fs::write(
            &cue,
            "TITLE \"Album\"\nFILE \"01 - Intro.wav\" WAVE\n\
             TRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 00:02:00\n",
        )
        .unwrap();
        let tracks = read_tracks(&[&cue]).remove(0);
        assert_eq!(tracks[0].shown_album(), "Album");
        assert_eq!(tracks[0].fallback_album(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
