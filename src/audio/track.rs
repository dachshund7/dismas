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
    tag::{ItemKey, ItemValue, Tag, TagItem},
    wavpack::WavPackFile,
};
use log::{debug, warn};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    audio::cover_cache::{AlbumCover, CoverArt, CoverCache},
    cue_sheet::{self, Claims, CueTrack, SplitSheet},
    i18n::{i18n, i18n_f},
    library::{folder_album_title, is_mbid},
    utils::{self, ATTRIBUTE_HOST_PATH},
};

mod credits;
mod file_name;
mod loader;
mod search;
mod tags;
mod work_hierarchy;

use credits::is_composer;
pub use credits::{MusicBrainzIds, TrackCredits};
pub use file_name::title_from_file_name;
use file_name::{fallback_album, title_from_name};
pub use loader::load_tracks;
pub use search::{normalize_search_text, search_key, tag_search_text};
pub use tags::{ExtraComments, file_tag, plain_commas, read_file_tags, track_title};
use tags::{track_album, track_artist};
use work_hierarchy::{MovementName, WorkHierarchy, WorkLabels};

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
    // Whether the file wasn't found, see `TrackData::missing()`
    missing: bool,
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

    // The title that searches look into: the one in the tags, or else the
    // one from the file name, but not the number of a track of a CUE sheet
    // without a title
    fn searched_title(&self) -> Option<&str> {
        match &self.title {
            Some(title) => Some(title),
            None if self.audio.is_none() => Some(&self.fallback_title),
            None => None,
        }
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

    // A track of a playlist whose file isn't there, e.g. on a drive that
    // isn't plugged in, with no tags but its name: it keeps its place in the
    // playlist, which would lose it the next time it's saved, and playback
    // skips it. The tracks of CUE sheets are named like the sheet names
    // tracks without a title
    pub fn missing(file: gio::File) -> Self {
        let path = file.path().unwrap_or_default();
        let (fallback_title, fallback_album) =
            if let Some((sheet, number)) = cue_sheet::missing_location_parts(&path) {
                (
                    i18n_f("Track {}", &[&number.to_string()]),
                    fallback_album(sheet),
                )
            } else {
                let stem = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy())
                    .unwrap_or_default();
                // The folder isn't there to compare the names with
                (title_from_name(&stem, |_| false), fallback_album(&path))
            };
        TrackData {
            artist: None,
            title: None,
            fallback_title,
            album: None,
            fallback_album,
            credits: TrackCredits::default(),
            tag_text: String::new(),
            cover: None,
            uuid: None,
            duration: 0,
            details: FileDetails::default(),
            file,
            audio: None,
            missing: true,
        }
    }

    pub fn is_missing(&self) -> bool {
        self.missing
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
            missing: false,
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
        // The file wasn't found when the playlist loaded
        #[property(name = "missing", get = Self::missing, type = bool)]
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
            if self.missing() {
                return i18n("File not found");
            }
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

        fn missing(&self) -> bool {
            self.data.borrow().is_missing()
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
        self.notify_missing();
    }

    // Makes the track one whose file wasn't found, like those of a
    // playlist that opens without their files, once its file turns out to
    // be gone, e.g. when it fails to play
    pub fn mark_missing(&self) {
        self.update(TrackData::missing(self.file()));
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

    // Whether the text that searching the queue looks into has all the
    // words, which are normalized like it. Searching tests every track of
    // the queue at each keystroke, so the text isn't copied
    pub fn matches_search(&self, words: &[String]) -> bool {
        self.with_search_key(|key| words.iter().all(|word| key.contains(word.as_str())))
    }

    #[cfg(test)]
    pub fn search_key(&self) -> String {
        self.with_search_key(ToString::to_string)
    }

    // The text that searching the queue looks into, normalized with
    // `normalize_search_text()`; computed once, until the tags change
    fn with_search_key<T>(&self, f: impl FnOnce(&str) -> T) -> T {
        let mut key = self.imp().search_key.borrow_mut();
        let key = key.get_or_insert_with(|| {
            let data = self.imp().data.borrow();
            // The album title is in the tags, without the names that
            // would make it match tracks by other people
            search_key(
                data.artist(),
                data.searched_title(),
                data.tag_text(),
                data.fallback_album(),
            )
        });
        f(key)
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

#[cfg(test)]
mod tests {
    use gtk::gio::prelude::*;
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::{
        MusicBrainzIds, Track, TrackCredits, TrackData, credits::Credit, track_album, track_title,
    };

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

    fn dates(date: Option<&str>, original_date: Option<&str>) -> TrackCredits {
        TrackCredits {
            date: date.map(str::to_string),
            original_date: original_date.map(str::to_string),
            ..Default::default()
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

    // The tracks read from some files, as the queue gets them
    fn read_tracks(files: &[&std::path::Path]) -> Vec<Vec<super::TrackData>> {
        use super::{Claims, TrackData, loader::FileCache};

        let cache = FileCache::new(Claims::new(files.iter().copied()));
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

    #[test]
    fn missing_files_are_named_after_their_paths() {
        let data = TrackData::missing(gtk::gio::File::for_path("/gone/Album/01 - Intro.flac"));
        assert!(data.is_missing());
        assert_eq!(data.shown_title(), "Intro");
        assert_eq!(data.shown_album(), "Album");
        assert_eq!(data.duration(), 0);
        assert_eq!(data.uri(), "file:///gone/Album/01%20-%20Intro.flac");

        // A track of a CUE sheet that isn't there either
        let data = TrackData::missing(gtk::gio::File::for_path("/gone/Album/Album.cue/track03"));
        assert_eq!(data.shown_title(), "Track 3");
        assert_eq!(data.shown_album(), "Album");
    }

    #[test]
    fn the_search_key_has_what_the_tags_and_names_say() {
        // No artist tag: "Unknown artist" is shown, but not searched, as in
        // the album grid
        let file = gtk::gio::File::for_path("/music/Fado/03 Barco Negro.flac");
        let track = Track::new(TrackData::missing(file));
        assert_eq!(track.artist(), "Unknown artist");
        let key = track.search_key();
        assert!(key.contains("barco negro"), "{key}");
        assert!(!key.contains("unknown"), "{key}");
        assert_eq!(
            key,
            super::search_key(None, Some("Barco Negro"), "", Some("Fado"))
        );
    }

    #[test]
    fn a_track_whose_file_is_gone_is_marked_missing() {
        let file = gtk::gio::File::for_path("/gone/Album/02 Second.flac");
        let mut data = TrackData::missing(file.clone());
        data.missing = false;
        let track = Track::new(data);
        assert!(!track.missing());

        track.mark_missing();
        assert!(track.missing());
        assert!(track.file().equal(&file));
        assert_eq!(track.title(), "Second");
    }
}
