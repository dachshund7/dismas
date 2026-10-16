// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    fmt::{self, Display, Formatter},
    fs::File,
    io::BufReader,
    path::PathBuf,
    time::Instant,
};

use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};
use itertools::Itertools;
use lofty::{
    config::ParseOptions,
    file::{AudioFile, FileType, TaggedFile},
    flac::FlacFile,
    ogg::{OpusFile, SpeexFile, VorbisComments, VorbisFile},
    prelude::{Accessor, TaggedFileExt},
    probe::Probe,
    tag::{ItemKey, Tag},
};
use log::{debug, warn};
use sha2::{Digest, Sha256};

use crate::{
    audio::{
        cover_cache::{CoverArt, CoverCache},
        youtube_description,
    },
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
    pub copyright: Vec<String>,
    pub musicbrainz: MusicBrainzIds,
}

// The ids of a track on MusicBrainz; they are only set if they are valid,
// so they can be put in a URL
#[derive(Debug, Clone, Default)]
pub struct MusicBrainzIds {
    pub recording: Option<String>,
    pub release: Option<String>,
    pub work: Option<String>,
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
        }
    }
}

impl TrackCredits {
    // The credits in the tags, filled in from the description that YouTube
    // generated, if the track has one; lofty leaves the tags it doesn't
    // know out of `tag`, so `synopsis` is the "SYNOPSIS" Vorbis comment
    fn from_tag(tag: &Tag, synopsis: Option<&str>) -> Self {
        let mut credits = Self::from_tag_items(tag);

        let description = synopsis
            .into_iter()
            .chain(tag.get_strings(ItemKey::Description))
            .chain(tag.get_strings(ItemKey::Comment))
            .find_map(youtube_description::parse);
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
            album_artist: tag
                .get_string(ItemKey::AlbumArtist)
                .map(ToString::to_string),
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
        }
    }

    // Takes what a YouTube description has and the tags don't
    fn fill_in(&mut self, description: TrackCredits) {
        fn fill<T>(field: &mut Vec<T>, other: Vec<T>) {
            if field.is_empty() {
                *field = other;
            }
        }

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

    // A single line with the conductors and performers, skipping the
    // names that are already displayed as the artist
    // The movement, with its number if any, e.g. "2. Adagio"; only set if
    // the track is part of a larger work, as tagged by the "Work & Movement"
    // option of MusicBrainz Picard
    pub fn movement_label(&self) -> Option<String> {
        self.work.as_ref()?;
        let movement = self.movement.as_deref()?;
        match self.movement_number.as_deref() {
            Some(n) if !n.is_empty() => Some(format!("{n}. {movement}")),
            _ => Some(movement.to_string()),
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

// Reads a file, and the "SYNOPSIS" Vorbis comment if it has one, which
// lofty leaves out of the tags of the `TaggedFile`
fn read_tagged_file(
    probe: Probe<BufReader<File>>,
) -> lofty::error::Result<(TaggedFile, Option<String>)> {
    fn read<F: AudioFile + Into<TaggedFile>>(
        probe: Probe<BufReader<File>>,
        comments: fn(&F) -> Option<&VorbisComments>,
    ) -> lofty::error::Result<(TaggedFile, Option<String>)> {
        let file = F::read_from(&mut probe.into_inner(), ParseOptions::new())?;
        let synopsis = comments(&file)
            .and_then(|c| c.get("SYNOPSIS"))
            .map(ToString::to_string);
        Ok((file.into(), synopsis))
    }

    match probe.file_type() {
        Some(FileType::Opus) => read(probe, |f: &OpusFile| Some(f.vorbis_comments())),
        Some(FileType::Vorbis) => read(probe, |f: &VorbisFile| Some(f.vorbis_comments())),
        Some(FileType::Speex) => read(probe, |f: &SpeexFile| Some(f.vorbis_comments())),
        Some(FileType::Flac) => read(probe, FlacFile::vorbis_comments),
        _ => probe.read().map(|f| (f, None)),
    }
}

// yt-dlp stores the date of the upload as "YYYYMMDD", which is not when
// the track was released
fn is_upload_date(date: &str) -> bool {
    date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit())
}

// The artists of a track, if the tags name any
fn track_artist(tag: &Tag) -> Option<String> {
    let artists = tag.get_strings(ItemKey::TrackArtist).join(", ");
    (!artists.is_empty()).then_some(artists)
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
    cover_art: Option<CoverArt>,
    cover_uuid: Option<String>,
    uuid: Option<String>,
    duration: u64,
    details: FileDetails,
    file: gio::File,
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

    pub fn cover_uuid(&self) -> Option<&str> {
        self.cover_uuid.as_deref()
    }

    pub fn duration(&self) -> u64 {
        self.duration
    }

    pub fn details(&self) -> &FileDetails {
        &self.details
    }

    pub fn cover_texture(&self) -> Option<&gdk::Texture> {
        if let Some(cover) = &self.cover_art {
            return Some(cover.texture());
        }

        None
    }

    pub fn cover_palette(&self) -> Option<&Vec<gdk::RGBA>> {
        if let Some(cover) = &self.cover_art {
            return Some(cover.palette());
        }

        None
    }

    pub fn cover_cache(&self) -> Option<&PathBuf> {
        if let Some(cover) = &self.cover_art {
            return cover.cache();
        }

        None
    }

    // Reads the tags and the cover of a track; this blocks, so it must not
    // be called on the main thread
    pub fn from_uri(uri: &str) -> Option<Self> {
        let now = Instant::now();

        let file = gio::File::for_uri(uri);
        // Files without a local path, like the ones in the trash or on the
        // web, can't be read by lofty
        let Some(path) = file.path() else {
            warn!("Unable to find a local path for {uri}");
            return None;
        };

        let (mime_type, _) = gio::content_type_guess(Some(&path), None);
        if !mime_type.starts_with("audio") || mime_type.ends_with("x-mpegurl") {
            return None;
        }

        let file_probe = match Probe::open(&path) {
            Ok(p) => p,
            Err(e) => {
                warn!("Unable to open file {path:?}: {e}");
                return None;
            }
        };

        let (tagged_file, synopsis) = match file_probe.guess_file_type() {
            Ok(t) => match read_tagged_file(t) {
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

        let mut artist = None;
        let mut title = None;
        let mut album = None;
        let mut credits = TrackCredits::default();
        let mut tag_text = String::new();
        let mut cover_art = None;
        let mut cover_uuid = None;
        let mut replay_gain = ReplayGain::default();
        if let Some(tag) = tagged_file.primary_tag() {
            debug!("Found primary tag");
            artist = track_artist(tag);
            title = tag.title().map(|s| s.to_string());
            album = tag.album().map(|s| s.to_string());
            credits = TrackCredits::from_tag(tag, synopsis.as_deref());
            replay_gain = ReplayGain::from_tag(tag);
            tag_text = tag_search_text(tag);
            if let Some(res) = CoverCache::cover_art(&path, tag) {
                cover_art = Some(res.0);
                cover_uuid = Some(res.1);
            }
        } else {
            warn!("Unable to load primary tag for: {uri}");
            for tag in tagged_file.tags() {
                debug!("Found tag: {:?}", tag.tag_type());
                // The tags that come later fill in for the ones missing
                // from the first
                artist = artist.or_else(|| track_artist(tag));
                title = title.or_else(|| tag.title().map(|s| s.to_string()));
                album = album.or_else(|| tag.album().map(|s| s.to_string()));
                credits = TrackCredits::from_tag(tag, synopsis.as_deref());
                replay_gain = ReplayGain::from_tag(tag);
                tag_text = tag_search_text(tag);
                if let Some(res) = CoverCache::cover_art(&path, tag) {
                    cover_art = Some(res.0);
                    cover_uuid = Some(res.1);
                }

                if artist.is_some() && title.is_some() {
                    break;
                }
            }
        }

        let info = file
            .query_info(
                &format!("standard::display-name,standard::size,{ATTRIBUTE_HOST_PATH}"),
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .ok();

        let uuid = info.as_ref().map(|info| {
            let mut hasher = Sha256::new();

            hasher.update(info.display_name().as_str());

            if let Some(ref artist) = artist {
                hasher.update(artist);
            }
            if let Some(ref title) = title {
                hasher.update(title);
            }
            if let Some(ref album) = album {
                hasher.update(album);
            }

            format!("{:x}", hasher.finalize())
        });

        let properties = tagged_file.properties();
        let duration = properties.duration().as_secs();
        let details = FileDetails {
            format: format_name(tagged_file.file_type()),
            path: info
                .as_ref()
                .and_then(|info| info.attribute_as_string(ATTRIBUTE_HOST_PATH))
                .map(|p| PathBuf::from(p.as_str()))
                .or(Some(path)),
            size: info
                .as_ref()
                .and_then(|info| u64::try_from(info.size()).ok()),
            bitrate: properties.audio_bitrate().filter(|b| *b > 0),
            sample_rate: properties.sample_rate().filter(|r| *r > 0),
            bit_depth: properties.bit_depth().filter(|d| *d > 0),
            channels: properties.channels().filter(|c| *c > 0),
            replay_gain,
        };

        debug!(
            "Track {:?} ('{:?}') loading time: {} ms",
            uuid,
            title,
            now.elapsed().as_millis()
        );

        Some(TrackData {
            artist,
            title,
            album,
            credits,
            tag_text,
            cover_art,
            cover_uuid,
            uuid,
            duration,
            details,
            file,
        })
    }

    pub fn uri(&self) -> String {
        self.file.uri().to_string()
    }

    pub fn file(&self) -> gio::File {
        self.file.clone()
    }
}

// Reads the tracks in a separate thread, and sends them one at a time, in
// order, with `None` for the files that are not tracks; reading stops if
// the receiver is dropped
pub fn load_tracks(files: Vec<gio::File>) -> async_channel::Receiver<Option<TrackData>> {
    let (sender, receiver) = async_channel::unbounded();

    std::thread::spawn(move || {
        for file in files {
            if sender
                .send_blocking(TrackData::from_uri(file.uri().as_str()))
                .is_err()
            {
                break;
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
            cover_art: None,
            cover_uuid: None,
            uuid: None,
            duration: 0,
            details: FileDetails::default(),
            file: gio::File::for_path("/does-not-exist"),
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
        // composer, as the work is in the header of their section; otherwise
        // the title and the artist
        #[property(name = "row-title", get = Self::row_title, type = String)]
        #[property(name = "row-subtitle", get = Self::row_subtitle, type = String)]
        #[property(name = "duration", get = Self::duration, type = u64)]
        #[property(name = "cover", get = Self::cover, nullable, type = Option<gdk::Texture>)]
        pub data: RefCell<TrackData>,
        #[property(get, set = Self::set_playing, explicit_notify)]
        pub playing: Cell<bool>,
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub selected: Cell<bool>,
        pub search_key: std::cell::OnceCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Track {
        const NAME: &'static str = "AmberolTrack";
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
            self.movement().unwrap_or_else(|| self.title())
        }

        fn row_subtitle(&self) -> String {
            match self.movement() {
                Some(_) => self.composer(),
                None => self.artist(),
            }
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

    // The composers, or the artist if the track has no composer
    pub fn composer(&self) -> String {
        self.imp().composer()
    }

    // The text that searching the queue looks into, normalized with
    // `normalize_search_text()`; computed once, as the tags don't change
    pub fn search_key(&self) -> String {
        self.imp()
            .search_key
            .get_or_init(|| {
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
}

impl Default for Track {
    fn default() -> Self {
        Self::empty()
    }
}

impl Display for Track {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(
            f,
            "Track {{ uuid: {}, uri: {}, artist: '{}', title: '{}' }}",
            self.uuid().unwrap(),
            self.uri(),
            self.artist(),
            self.title()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{TrackCredits, album_title_search_text, normalize_search_text};

    fn dates(date: Option<&str>, original_date: Option<&str>) -> TrackCredits {
        TrackCredits {
            date: date.map(str::to_string),
            original_date: original_date.map(str::to_string),
            ..Default::default()
        }
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
}
