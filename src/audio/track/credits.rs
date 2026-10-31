// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The credits in the tags of a track, and how they're shown: the work
//! and movement, the composers, the conductors and the performers

use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Credit {
    pub name: String,
    pub role: Option<String>,
}

impl Credit {
    // MusicBrainz Picard stores performers as "Name (role)"
    pub(super) fn parse(s: &str) -> Self {
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

// Who a performer the credits name is, which the list of performers is
// divided by, in this order
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PerformerKind {
    Conductor,
    Ensemble,
    Soloist,
}

// A performer the credits name, with the role the tags give them, e.g.
// "piano" or "orchestra"
#[derive(Debug, Clone, PartialEq)]
pub struct ListedPerformer {
    pub kind: PerformerKind,
    pub name: String,
    pub role: Option<String>,
}

// The additional credits and release information in the tags, mostly
// relevant for classical music
#[derive(Debug, Clone, Default)]
pub struct TrackCredits {
    pub work: Option<String>,
    // The full title of the work, when it's part of a larger one, e.g.
    // "The Well-Tempered Clavier, Book I: Prelude and Fugue no. 1 in C
    // major, BWV 846"
    pub work_title: Option<String>,
    // The MusicBrainz id of the work, from the "Work Hierarchy" plugin
    pub work_id: Option<String>,
    // The work it's part of, or the collection it's in, from the "Work
    // Hierarchy" plugin
    pub work_collection: Option<String>,
    // The work it arranges, if MusicBrainz has it as an arrangement, from
    // the "Work Hierarchy" plugin
    pub work_arrangement: Option<Arrangement>,
    // The MusicBrainz ids of the parts of the work the track performs, at
    // every level below the work, from the "Work Hierarchy" plugin, which
    // an arrangement of one of them finds the work by
    pub work_part_ids: Vec<String>,
    // Where the movement is in the work, which tells two recordings of a
    // work on one album apart: the positions in each level below the work,
    // empty if they aren't known, and `None` if the track performs the
    // whole work or isn't part of one
    pub movement_order: Option<Vec<u32>>,
    // The names of the works in the tag of the "Work Hierarchy" plugin,
    // which lofty doesn't search, see `WorkHierarchy::search_text()`
    pub work_search: String,
    pub movement: Option<String>,
    pub movement_number: Option<String>,
    // The levels of the work above the movement, and the movement, when
    // the movement is part of a part of the work, e.g. "Book I: Prelude
    // and Fugue no. 1 in C major" and "1. Prelude"; the queue shows them
    // on separate lines, as the movement would be cut off
    pub movement_levels: Option<(String, String)>,
    pub composers: Vec<String>,
    // The sort names of the composers, e.g. "Bach, Johann Sebastian", as
    // the tags give them; they go with the composers only when there are
    // as many of each, see `library::credited_work_composers()`
    pub composer_sorts: Vec<String>,
    // The name the artist is sorted by, e.g. "Beatles, The", as the tags
    // give it
    pub artist_sort: Option<String>,
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
    // The date of the release from the release date tag, which taggers
    // other than Picard write
    pub release_date: Option<String>,
    // When the track was recorded, e.g. "1937-11-08/1938-03-08", which the
    // "Work Hierarchy" plugin writes from MusicBrainz
    pub recording_date: Option<String>,
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
    pub fn from_tag(tag: &Tag, extra: &ExtraComments) -> Self {
        let mut credits = Self::from_tag_items(tag);
        credits.recording_date = extra
            .recording_date
            .as_deref()
            .map(str::trim)
            .filter(|date| !date.is_empty())
            .map(ToString::to_string);
        let hierarchy = extra
            .work_hierarchy
            .as_deref()
            .and_then(WorkHierarchy::parse);
        credits.composer_ids = hierarchy.as_ref().and_then(WorkHierarchy::composer_ids);
        credits.work_search = hierarchy
            .as_ref()
            .map(WorkHierarchy::search_text)
            .unwrap_or_default();
        // The hierarchy has the numbers of every level, so the movement
        // includes them where they belong, whatever the work and movement
        // tags say. A track of a whole part with a catalogue number of its
        // own, a Prelude and Fugue in one track, is not a movement
        if let Some(WorkLabels {
            work,
            title,
            movement,
            composers,
            id,
            collection,
            order,
            arrangement,
            part_ids,
        }) = hierarchy.as_ref().and_then(WorkHierarchy::labels)
        {
            credits.work_arrangement = arrangement;
            credits.work_part_ids = part_ids;
            credits.work_title = (title != work).then_some(title);
            credits.work = Some(work);
            credits.work_composers = composers;
            credits.work_id = Some(id);
            credits.work_collection = collection;
            credits.movement_order = order;
            let (movement, levels) = movement.map(MovementName::into_names).unzip();
            credits.movement = movement;
            credits.movement_number = None;
            credits.movement_levels = levels.flatten();
        } else if let Some(work_id) = credits.musicbrainz.work.as_deref() {
            // A whole work in a track, which the work tags name, or an
            // arrangement of a single movement, which the tags make a
            // movement of the work it's part of
            credits.work_arrangement = hierarchy
                .as_ref()
                .and_then(|hierarchy| hierarchy.linked_work_arrangement(work_id));
        }

        credits
    }

    fn from_tag_items(tag: &Tag) -> Self {
        let work = tag.get_string(ItemKey::Work).map(ToString::to_string);
        let movement = tag.get_string(ItemKey::Movement).map(ToString::to_string);
        let movement_number = tag
            .get_string(ItemKey::MovementNumber)
            .map(ToString::to_string);
        let movement_order = (work.is_some() && movement.is_some()).then(|| {
            movement_number
                .as_deref()
                .and_then(|n| n.trim().parse().ok())
                .into_iter()
                .collect()
        });
        TrackCredits {
            work,
            work_title: None,
            work_id: None,
            work_collection: None,
            work_arrangement: None,
            work_part_ids: Vec::new(),
            movement_order,
            work_search: String::new(),
            movement,
            movement_number,
            movement_levels: None,
            composers: split_composers(tag.get_strings(ItemKey::Composer)),
            composer_sorts: split_composers(tag.get_strings(ItemKey::ComposerSortOrder)),
            artist_sort: tag
                .get_string(ItemKey::TrackArtistSortOrder)
                .map(str::trim)
                .filter(|sort| !sort.is_empty())
                .map(ToString::to_string),
            work_composers: Vec::new(),
            lyricists: tag_strings(tag, ItemKey::Lyricist),
            arrangers: tag_strings(tag, ItemKey::Arranger),
            conductors: tag_strings(tag, ItemKey::Conductor),
            performers: tag_strs(tag, ItemKey::Performer)
                .map(Credit::parse)
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
            release_date: tag
                .get_string(ItemKey::ReleaseDate)
                .map(ToString::to_string),
            recording_date: None,
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

    // The year the track was made, which the albums are sorted by and
    // which is shown with them: the year it was recorded, or else the year
    // it was first released, as the date of a reissue says nothing of the
    // recording, or else the year of the release
    pub fn year(&self) -> Option<String> {
        self.year_date()
            .and_then(|date| date.get(..4))
            .map(ToString::to_string)
    }

    // The date `year()` takes the year from, as the tag has it, e.g.
    // "1937-11-08/1938-03-08" for a recording made over several months
    pub fn year_date(&self) -> Option<&str> {
        [
            &self.recording_date,
            &self.original_date,
            &self.date,
            &self.release_date,
        ]
        .into_iter()
        .flatten()
        .map(|date| date.trim())
        .find(|date| {
            date.get(..4)
                .is_some_and(|year| year.chars().all(|c| c.is_ascii_digit()))
        })
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
        self.listed_composer_names(artist)
            .map(|composers| composers.join(", "))
    }

    // The composers `listed_composers()` names, one by one
    pub fn listed_composer_names(&self, artist: Option<&str>) -> Option<&[String]> {
        let classical = self
            .genres
            .iter()
            .any(|g| g.eq_ignore_ascii_case("classical"));
        let named = artist.is_some_and(|a| self.artist_is_composer(a));
        if !classical || self.composers.is_empty() || named {
            return None;
        }
        Some(&self.composers)
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

    // Whether the track is a movement of a work, which `movement_label()`
    // names
    pub fn is_movement(&self) -> bool {
        self.work.is_some() && self.movement.is_some()
    }

    // The work the track is a movement of, if it is one
    pub fn movement_work(&self) -> Option<&str> {
        self.work.as_deref().filter(|_| self.is_movement())
    }

    // The levels of the work above the movement, and the movement, if the
    // track is a movement that the queue shows them for, on lines of their
    // own
    pub fn shown_levels(&self) -> Option<&(String, String)> {
        self.movement_levels.as_ref().filter(|_| self.is_movement())
    }

    // The composers of the work of a movement, which all its movements
    // share, or else the track's
    pub fn work_composer_names(&self) -> &[String] {
        if self.work_composers.is_empty() {
            &self.composers
        } else {
            &self.work_composers
        }
    }

    // The performers to show in the queue for a track that is not part of
    // a larger work, when the artist is its composer, as classical
    // releases are often tagged; the artist would be the only name shown.
    // They come from the tags, or else from the album artist. Not when the
    // artist performs too, as songwriters do, whose band would only
    // clutter the row
    pub fn performers_of_composer(&self, artist: Option<&str>) -> Option<String> {
        let artist = artist.filter(|a| self.only_names_composer(a))?;
        Some(self.summary(Some(artist)))
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.album_performers_of_composer(artist)
                    .map(ToString::to_string)
            })
    }

    // The performers `performers_of_composer()` names, one by one; the
    // performers in the album artist are one name, as they are written
    pub fn performer_names_of_composer(&self, artist: Option<&str>) -> Option<Vec<&str>> {
        let artist = artist.filter(|a| self.only_names_composer(a))?;
        let (conductors, performers) = self.summary_names(Some(artist));
        let names = conductors.into_iter().chain(performers).collect::<Vec<_>>();
        if names.is_empty() {
            self.album_performers_of_composer(artist).map(|p| vec![p])
        } else {
            Some(names)
        }
    }

    // Whether the artist is the composer, and none of the performers
    fn only_names_composer(&self, artist: &str) -> bool {
        // Without composers in the tags, the album artist tells whether the
        // artist is the composer
        let is_composer = if self.composers.is_empty() {
            self.album_performers_of_composer(artist).is_some()
        } else {
            self.artist_is_composer(artist)
        };
        is_composer
            && !self
                .performers
                .iter()
                .any(|p| artist.contains(p.name.as_str()))
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
        let (conductors, performers) = self.summary_names(artist);
        [conductors.join(", "), performers.join(", ")]
            .iter()
            .filter(|s| !s.is_empty())
            .join(" · ")
    }

    // The conductors and the performers that `summary()` names, apart
    fn summary_names(&self, artist: Option<&str>) -> (Vec<&str>, Vec<&str>) {
        let ensemble = self.ensemble_in_album_artist();
        let conductors = self
            .conductors
            .iter()
            .map(String::as_str)
            .filter(|c| Some(*c) != artist && !ensemble.is_some_and(|e| e.contains(c)))
            .collect();
        let performers = match ensemble {
            Some(ensemble) if Some(ensemble) == artist => Vec::new(),
            Some(ensemble) => vec![ensemble],
            None => self
                .featured_performers()
                .into_iter()
                .map(|p| p.name.as_str())
                .filter(|name| Some(*name) != artist && !self.conductors.iter().any(|c| c == name))
                .unique()
                .collect(),
        };
        (conductors, performers)
    }

    // The conductors and performers that the credits line names, for the
    // list of performers: the same ones as `summary()`, and not the
    // players of a group, which Picard lists with it. Unlike there, the
    // artist is kept, as it's one of the performers
    pub fn listed_performers(&self) -> Vec<ListedPerformer> {
        let mut listed = self
            .conductors
            .iter()
            .map(|name| ListedPerformer {
                kind: PerformerKind::Conductor,
                name: name.clone(),
                role: None,
            })
            .collect::<Vec<_>>();

        if let Some(ensemble) = self.ensemble_in_album_artist() {
            // The names in the album artist that the tags credit are the
            // conductors and players they say; the others are taken to be
            // groups, although they can be players that the tags of other
            // tracks credit, which the list of performers puts right
            for name in ensemble.split(", ").flat_map(|part| part.split(" & ")) {
                let name = name.trim();
                if name.is_empty() || self.conductors.iter().any(|c| c == name) {
                    continue;
                }
                let performer = self.performers.iter().find(|p| p.name == name);
                listed.push(ListedPerformer {
                    kind: if performer.is_some() {
                        PerformerKind::Soloist
                    } else {
                        PerformerKind::Ensemble
                    },
                    name: name.to_string(),
                    role: performer.and_then(|p| p.role.clone()),
                });
            }
            return listed;
        }

        listed.extend(
            self.featured_performers()
                .into_iter()
                .map(|performer| ListedPerformer {
                    kind: if self.is_group(performer) {
                        PerformerKind::Ensemble
                    } else {
                        PerformerKind::Soloist
                    },
                    name: performer.name.clone(),
                    role: performer.role.clone(),
                }),
        );
        listed
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
pub(super) fn is_composer(name: &str, composer: &str) -> bool {
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

// The names of composers, or their sort names, in the values of a tag,
// which can also be separated by semicolons, as Mp3tag and foobar2000
// write them, or joined by " & " or " / ", as iTunes and others do, e.g.
// "John Lennon & Paul McCartney" or "Lennon / McCartney", without repeats.
// Only with spaces around them, so that "AC/DC" stays whole, and not by
// commas, which a sort name has, as in "Bach, Johann Sebastian"
pub fn split_composers<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    values
        .flat_map(|value| value.split(';'))
        .flat_map(|value| value.split(" & "))
        .flat_map(|value| value.split(" / "))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unique()
        .map(ToString::to_string)
        .collect()
}

// All the values for a key, including the ones that were stored as a
// single value separated by semicolons
fn tag_strings(tag: &Tag, key: ItemKey) -> Vec<String> {
    tag_strs(tag, key).map(ToString::to_string).collect()
}

fn tag_strs(tag: &Tag, key: ItemKey) -> impl Iterator<Item = &str> {
    tag.get_strings(key)
        .flat_map(|s| s.split(';'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unique()
}
