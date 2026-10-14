// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    fmt::{self, Display, Formatter},
    path::PathBuf,
    time::Instant,
};

use glib::{ParamSpec, ParamSpecBoolean, ParamSpecObject, ParamSpecString, ParamSpecUInt, Value};
use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};
use itertools::Itertools;
use lofty::{
    prelude::{Accessor, TaggedFileExt},
    probe::Probe,
    tag::{ItemKey, Tag},
};
use log::{debug, warn};
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};

use crate::{
    audio::cover_cache::{CoverArt, CoverCache},
    i18n::i18n,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Performer {
    pub name: String,
    pub role: Option<String>,
}

impl Performer {
    // MusicBrainz Picard stores performers as "Name (role)"
    fn parse(s: &str) -> Self {
        if let Some(name) = s.strip_suffix(')') {
            if let Some((name, role)) = name.rsplit_once(" (") {
                return Performer {
                    name: name.trim().to_string(),
                    role: Some(role.trim().to_string()),
                };
            }
        }

        Performer {
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
    pub performers: Vec<Performer>,
    pub album_artist: Option<String>,
    pub date: Option<String>,
    pub labels: Vec<String>,
    pub catalog_numbers: Vec<String>,
    pub genres: Vec<String>,
}

impl TrackCredits {
    fn from_tag(tag: &Tag) -> Self {
        TrackCredits {
            work: tag.get_string(ItemKey::Work).map(|s| s.to_string()),
            movement: tag.get_string(ItemKey::Movement).map(|s| s.to_string()),
            movement_number: tag
                .get_string(ItemKey::MovementNumber)
                .map(|s| s.to_string()),
            composers: tag_strings(tag, ItemKey::Composer),
            lyricists: tag_strings(tag, ItemKey::Lyricist),
            arrangers: tag_strings(tag, ItemKey::Arranger),
            conductors: tag_strings(tag, ItemKey::Conductor),
            performers: tag_strings(tag, ItemKey::Performer)
                .iter()
                .map(|s| Performer::parse(s))
                .collect(),
            album_artist: tag.get_string(ItemKey::AlbumArtist).map(|s| s.to_string()),
            date: tag
                .get_string(ItemKey::RecordingDate)
                .or_else(|| tag.get_string(ItemKey::Year))
                .map(|s| s.to_string()),
            labels: tag_strings(tag, ItemKey::Label),
            catalog_numbers: tag_strings(tag, ItemKey::CatalogNumber),
            genres: tag_strings(tag, ItemKey::Genre),
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

// The text of all the tags of a track, for searching the playlist; this
// leaves out the tags that only contain identifiers, numbers or technical
// data, as they would make short searches, like "op 29", match by accident
fn tag_search_text(tag: &Tag) -> String {
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

// All the values for a key, including the ones that were stored as a
// single value separated by semicolons
fn tag_strings(tag: &Tag, key: ItemKey) -> Vec<String> {
    tag.get_strings(key)
        .flat_map(|s| s.split(';'))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unique()
        .map(|s| s.to_string())
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
    file: gio::File,
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

    pub fn from_uri(uri: &str) -> Self {
        let now = Instant::now();

        let file = gio::File::for_uri(uri);
        let path = file.path().expect("Unable to find file");

        let (mime_type, _) = gio::content_type_guess(Some(&path), None);
        if !mime_type.starts_with("audio") || mime_type.ends_with("x-mpegurl") {
            return TrackData::default();
        }

        let file_probe = match Probe::open(&path) {
            Ok(p) => p,
            Err(e) => {
                warn!("Unable to open file {:?}: {}", path, e);
                return TrackData::default();
            }
        };

        let tagged_file = match file_probe.guess_file_type() {
            Ok(t) => match t.read() {
                Ok(f) => f,
                Err(e) => {
                    warn!("Unable to read parsed file {:?}: {}", path, e);
                    return TrackData::default();
                }
            },
            Err(e) => {
                warn!("Unable to parse file type {:?}: {}", path, e);
                return TrackData::default();
            }
        };

        let mut cover_cache = CoverCache::global().lock().unwrap();

        let mut artist = None;
        let mut title = None;
        let mut album = None;
        let mut credits = TrackCredits::default();
        let mut tag_text = String::new();
        let mut cover_art = None;
        let mut cover_uuid = None;
        if let Some(tag) = tagged_file.primary_tag() {
            debug!("Found primary tag");
            artist = Some(tag.get_strings(ItemKey::TrackArtist).join(", "));
            title = tag.title().map(|s| s.to_string());
            album = tag.album().map(|s| s.to_string());
            credits = TrackCredits::from_tag(tag);
            tag_text = tag_search_text(tag);
            if let Some(res) = cover_cache.cover_art(&path, tag) {
                cover_art = Some(res.0);
                cover_uuid = Some(res.1);
            }
        } else {
            warn!("Unable to load primary tag for: {}", uri);
            for tag in tagged_file.tags() {
                debug!("Found tag: {:?}", tag.tag_type());
                artist = Some(tag.get_strings(ItemKey::TrackArtist).join(", "));
                title = tag.title().map(|s| s.to_string());
                album = tag.album().map(|s| s.to_string());
                credits = TrackCredits::from_tag(tag);
                tag_text = tag_search_text(tag);
                if let Some(res) = cover_cache.cover_art(&path, tag) {
                    cover_art = Some(res.0);
                    cover_uuid = Some(res.1);
                }

                if artist.is_some() && title.is_some() {
                    break;
                }
            }
        };

        let uuid = match file.query_info(
            "standard::display-name",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        ) {
            Ok(info) => {
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

                Some(format!("{:x}", hasher.finalize()))
            }
            _ => None,
        };

        let properties = lofty::prelude::AudioFile::properties(&tagged_file);
        let duration = properties.duration().as_secs();

        debug!(
            "Track {:?} ('{:?}') loading time: {} ms",
            &uuid,
            &title,
            now.elapsed().as_millis()
        );

        TrackData {
            artist,
            title,
            album,
            credits,
            tag_text,
            cover_art,
            cover_uuid,
            uuid,
            duration,
            file,
        }
    }

    pub fn uri(&self) -> String {
        self.file.uri().to_string()
    }

    pub fn file(&self) -> gio::File {
        self.file.clone()
    }
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
            file: gio::File::for_path("/does-not-exist"),
        }
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct Track {
        pub data: RefCell<TrackData>,
        pub playing: Cell<bool>,
        pub selected: Cell<bool>,
        pub search_key: std::cell::OnceCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Track {
        const NAME: &'static str = "AmberolTrack";
        type Type = super::Track;
    }

    impl ObjectImpl for Track {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").construct_only().build(),
                    ParamSpecString::builder("artist").read_only().build(),
                    ParamSpecString::builder("title").read_only().build(),
                    ParamSpecString::builder("album").read_only().build(),
                    ParamSpecString::builder("credits").read_only().build(),
                    ParamSpecString::builder("heading").read_only().build(),
                    ParamSpecString::builder("movement").read_only().build(),
                    ParamSpecString::builder("row-title").read_only().build(),
                    ParamSpecString::builder("row-subtitle").read_only().build(),
                    ParamSpecString::builder("row-composer").read_only().build(),
                    ParamSpecUInt::builder("duration").read_only().build(),
                    ParamSpecObject::builder::<gdk::Texture>("cover")
                        .read_only()
                        .build(),
                    ParamSpecBoolean::builder("playing").build(),
                    ParamSpecBoolean::builder("selected").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &Value, pspec: &ParamSpec) {
            match pspec.name() {
                "uri" => {
                    let obj = self.obj();
                    if let Ok(p) = value.get::<&str>() {
                        self.data.replace(TrackData::from_uri(p));
                        obj.notify("artist");
                        obj.notify("title");
                        obj.notify("album");
                        obj.notify("credits");
                        obj.notify("heading");
                        obj.notify("movement");
                        obj.notify("row-title");
                        obj.notify("row-subtitle");
                        obj.notify("row-composer");
                        obj.notify("duration");
                        obj.notify("cover");
                    }
                }
                "playing" => {
                    let p = value.get::<bool>().expect("Value must be a boolean");
                    self.playing.set(p);
                }
                "selected" => {
                    let p = value.get::<bool>().expect("Value must be a boolean");
                    self.selected.set(p);
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> Value {
            let obj = self.obj();
            match pspec.name() {
                "artist" => obj.artist().to_value(),
                "title" => obj.title().to_value(),
                "album" => obj.album().to_value(),
                "credits" => obj.credits().to_value(),
                "heading" => obj.heading().to_value(),
                "movement" => obj.movement().unwrap_or_default().to_value(),
                "row-title" => obj.row_title().to_value(),
                "row-subtitle" => obj.row_subtitle().to_value(),
                "row-composer" => obj.row_composer().to_value(),
                "duration" => obj.duration().to_value(),
                "uri" => obj.uri().to_value(),
                "cover" => obj.cover_texture().to_value(),
                "playing" => self.playing.get().to_value(),
                "selected" => self.selected.get().to_value(),
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct Track(ObjectSubclass<imp::Track>);
}

impl Track {
    pub fn new(uri: &str) -> Self {
        glib::Object::builder::<Self>().property("uri", uri).build()
    }

    pub fn from_uri(uri: &str) -> Result<Track, &'static str> {
        let res = Track::new(uri);
        if res.equals(&Track::default()) {
            Err("Invalid track")
        } else {
            Ok(res)
        }
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

    pub fn uri(&self) -> String {
        self.imp().data.borrow().uri()
    }

    pub fn artist(&self) -> String {
        match self.imp().data.borrow().artist() {
            Some(artist) => artist.to_string(),
            None => i18n("Unknown artist"),
        }
    }

    pub fn title(&self) -> String {
        match self.imp().data.borrow().title() {
            Some(title) => title.to_string(),
            None => i18n("Unknown title"),
        }
    }

    pub fn album(&self) -> String {
        match self.imp().data.borrow().album() {
            Some(album) => album.to_string(),
            None => i18n("Unknown album"),
        }
    }

    // The conductors and performers, as a single line
    pub fn credits(&self) -> String {
        let data = self.imp().data.borrow();
        data.credits().summary(data.artist())
    }

    pub fn credit_details(&self) -> TrackCredits {
        self.imp().data.borrow().credits().clone()
    }

    pub fn cover_texture(&self) -> Option<gdk::Texture> {
        self.imp().data.borrow().cover_texture().cloned()
    }

    pub fn cover_color(&self) -> Option<gdk::RGBA> {
        self.imp().data.borrow().cover_palette().map(|p| p[0])
    }

    pub fn cover_palette(&self) -> Option<Vec<gdk::RGBA>> {
        self.imp().data.borrow().cover_palette().cloned()
    }

    pub fn cover_uuid(&self) -> Option<String> {
        self.imp().data.borrow().cover_uuid().map(|s| s.to_string())
    }

    pub fn cover_cache(&self) -> Option<PathBuf> {
        self.imp().data.borrow().cover_cache().cloned()
    }

    pub fn duration(&self) -> u64 {
        self.imp().data.borrow().duration()
    }

    pub fn playing(&self) -> bool {
        self.imp().playing.get()
    }

    pub fn set_playing(&self, playing: bool) {
        let was_playing = self.imp().playing.replace(playing);
        if was_playing != playing {
            self.notify("playing");
        }
    }

    pub fn selected(&self) -> bool {
        self.imp().selected.get()
    }

    pub fn set_selected(&self, selected: bool) {
        let was_selected = self.imp().selected.replace(selected);
        if was_selected != selected {
            self.notify("selected");
        }
    }

    pub fn uuid(&self) -> Option<String> {
        self.imp().data.borrow().uuid().map(|s| s.to_string())
    }

    // The movement of a larger work, if the track is one
    pub fn movement(&self) -> Option<String> {
        self.imp().data.borrow().credits().movement_label()
    }

    // The work the movement belongs to, if the track is a movement
    pub fn movement_work(&self) -> Option<String> {
        self.movement()?;
        self.imp().data.borrow().credits().work.clone()
    }

    // The main line in the track details: the work, for movements, and
    // the title otherwise
    pub fn heading(&self) -> String {
        self.movement_work().unwrap_or_else(|| self.title())
    }

    // The lines in the playlist: the movement and the work it belongs to,
    // as all the movements of a work share the start of their titles; or
    // the title and the artist
    pub fn row_title(&self) -> String {
        self.movement().unwrap_or_else(|| self.title())
    }

    pub fn row_subtitle(&self) -> String {
        self.movement_work().unwrap_or_else(|| self.artist())
    }

    // The third line in the playlist, only for movements, as the work
    // replaces the artist in the second line
    pub fn row_composer(&self) -> String {
        match self.movement() {
            Some(_) => self.composer(),
            None => String::new(),
        }
    }

    // The composers, or the artist if the track has no composer
    pub fn composer(&self) -> String {
        let composers = self.imp().data.borrow().credits().composers.join(", ");
        if composers.is_empty() {
            self.artist()
        } else {
            composers
        }
    }

    // The text that searching the playlist looks into, normalized with
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
    use super::{album_title_search_text, normalize_search_text};

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
