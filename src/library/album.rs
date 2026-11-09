// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    cmp::Ordering,
};

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::AlbumInfo;
use crate::tag_date::{DateKind, SortDates, TagDate};

// What the album grid is sorted by, as in the "album-sort" setting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AlbumSort {
    Artist,
    Title,
    Date(DateKind),
    Added,
}

impl AlbumSort {
    pub(crate) fn from_nick(nick: &str) -> Self {
        match nick {
            "title" => Self::Title,
            // The sort that was called Year keeps its name in the settings
            "year" => Self::Date(DateKind::Released),
            "original" => Self::Date(DateKind::OriginalRelease),
            "recorded" => Self::Date(DateKind::Recorded),
            "added" => Self::Added,
            _ => Self::Artist,
        }
    }

    // The newest albums are the ones people look for first, while names
    // read best from A to Z
    pub(crate) fn default_descending(self) -> bool {
        matches!(self, Self::Date(_) | Self::Added)
    }
}

// What tells a selected album apart: its id, and for a copy of an album
// that shows a recording of a work, see `Album::recording_copy()`, the URI
// of the recording's first track, as an album can have two recordings of a
// work, each with a cover of its own
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SelectionKey {
    pub(crate) album: String,
    pub(crate) recording: Option<String>,
}

// Compares two values in the given direction, with the missing ones last
// either way
fn cmp_present_first<T: Ord>(a: Option<&T>, b: Option<&T>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) if descending => b.cmp(a),
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

// Compares two dates in the given direction, by their years, then their
// months and then their days, each missing one last either way: a date with
// only a year goes after the others of that year, as an album without a
// date goes after the others
fn cmp_dates(a: Option<TagDate>, b: Option<TagDate>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => cmp_present_first(Some(&a.year), Some(&b.year), descending)
            .then_with(|| cmp_present_first(a.month.as_ref(), b.month.as_ref(), descending))
            .then_with(|| cmp_present_first(a.day.as_ref(), b.day.as_ref(), descending)),
        // A missing date last either way
        _ => a.is_none().cmp(&b.is_none()),
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::Album)]
    pub(crate) struct Album {
        #[property(get)]
        pub(crate) id: OnceCell<String>,
        #[property(get)]
        pub(crate) title: RefCell<String>,
        #[property(get)]
        pub(crate) artist: RefCell<String>,
        #[property(get)]
        pub(crate) year: RefCell<Option<String>>,
        #[property(get)]
        pub(crate) cover_path: OnceCell<Option<String>>,
        // For a copy that shows a recording of a work, the URI of its first
        // track, see `Album::recording_copy()`
        pub(crate) recording: OnceCell<String>,
        // The record label, and the number it gives the release
        #[property(get)]
        pub(crate) label: RefCell<Option<String>>,
        #[property(get)]
        pub(crate) catalog_number: RefCell<Option<String>>,
        // Whether the album is selected in the grid
        #[property(get, set)]
        pub(crate) selected: Cell<bool>,
        // While a search of the grid matches only some of the tracks, the
        // number that do, and the number of all of them; otherwise 0
        #[property(get, set)]
        pub(crate) matching_tracks: Cell<u32>,
        #[property(get, set)]
        pub(crate) track_count: Cell<u32>,
        // Whether the album is marked as a favorite, which the grid shows
        // with a heart on its cover
        #[property(get, set)]
        pub(crate) favorite: Cell<bool>,

        // The keys the grid is sorted by, in the order of the locale
        pub(crate) artist_key: OnceCell<glib::CollationKey>,
        pub(crate) title_key: OnceCell<glib::CollationKey>,
        pub(crate) added: Cell<Option<i64>>,
        pub(crate) dates: Cell<SortDates>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Album {
        const NAME: &'static str = "DismasAlbum";
        type Type = super::Album;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Album {}
}

glib::wrapper! {
    pub(crate) struct Album(ObjectSubclass<imp::Album>);
}

impl Album {
    pub(crate) fn new(info: AlbumInfo) -> Self {
        let album: Self = glib::Object::new();
        let imp = album.imp();
        let _ = imp.id.set(info.id);
        imp.title.replace(info.title);
        imp.artist.replace(info.artist);
        imp.year.replace(info.year);
        let _ = imp.cover_path.set(info.cover);
        imp.label.replace(info.label);
        imp.catalog_number.replace(info.catalog_number);
        imp.added.set(info.added);
        imp.dates.set(info.dates);
        let _ = imp
            .artist_key
            .set(glib::CollationKey::from(info.artist_sort.as_str()));
        let _ = imp
            .title_key
            .set(glib::CollationKey::from(imp.title.borrow().as_str()));
        album
    }

    // A copy of the album for the cover of a recording of a work on it,
    // which is selected on its own, as the album can have two, and shows
    // the album's heart. It isn't sorted, so it has no sort keys
    pub(crate) fn recording_copy(&self, uri: &str) -> Self {
        let copy: Self = glib::Object::new();
        let (from, to) = (self.imp(), copy.imp());
        let _ = to.id.set(self.id());
        to.title.replace(self.title());
        to.artist.replace(self.artist());
        to.year.replace(self.year());
        let _ = to.cover_path.set(self.cover_path());
        to.label.replace(self.label());
        to.catalog_number.replace(self.catalog_number());
        to.added.set(from.added.get());
        let _ = to.recording.set(uri.to_owned());
        self.bind_property("favorite", &copy, "favorite")
            .sync_create()
            .build();
        copy
    }

    // What tells it apart among the selected albums
    pub(crate) fn selection_key(&self) -> SelectionKey {
        SelectionKey {
            album: self.id(),
            recording: self.imp().recording.get().cloned(),
        }
    }

    // The id and the cover, borrowed: the grid's search and the covers read
    // ahead look at many albums at a time
    pub(crate) fn id_str(&self) -> &str {
        self.imp().id.get().map_or("", String::as_str)
    }

    pub(crate) fn cover_path_str(&self) -> Option<&str> {
        self.imp().cover_path.get().and_then(Option::as_deref)
    }

    // The order of two albums in the grid; the albums that tie are always
    // sorted from A to Z, and the oldest first
    pub(crate) fn compare(&self, other: &Self, sort: AlbumSort, descending: bool) -> Ordering {
        let (a, b) = (self.imp(), other.imp());
        let artist =
            |descending| cmp_present_first(a.artist_key.get(), b.artist_key.get(), descending);
        let title =
            |descending| cmp_present_first(a.title_key.get(), b.title_key.get(), descending);
        let (a_dates, b_dates) = (a.dates.get(), b.dates.get());
        let date = |kind, descending| cmp_dates(a_dates.get(kind), b_dates.get(kind), descending);
        // The ties of the other sorts are by when the music was made, so
        // that an artist's albums are in the order they were recorded
        let recorded = |descending| date(DateKind::Recorded, descending);
        let added =
            || cmp_present_first(a.added.get().as_ref(), b.added.get().as_ref(), descending);

        match sort {
            AlbumSort::Artist => artist(descending)
                .then_with(|| recorded(false))
                .then_with(|| title(false)),
            AlbumSort::Title => title(descending)
                .then_with(|| artist(false))
                .then_with(|| recorded(false)),
            AlbumSort::Date(kind) => date(kind, descending)
                .then_with(|| artist(false))
                .then_with(|| title(false)),
            AlbumSort::Added => added()
                .then_with(|| artist(false))
                .then_with(|| title(false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(artist: &str, title: &str, added: Option<i64>) -> AlbumInfo {
        AlbumInfo {
            id: format!("{artist} {title}"),
            title: title.to_string(),
            artist: artist.to_string(),
            artist_sort: artist.to_string(),
            year: None,
            dates: SortDates::default(),
            added,
            cover: None,
            label: None,
            catalog_number: None,
        }
    }

    // An album whose three dates are the same, as in a library with only
    // the Date tag
    fn album(artist: &str, title: &str, year: Option<&str>, added: Option<i64>) -> Album {
        let date = year.unwrap_or_default();
        Album::new(AlbumInfo {
            year: year.map(str::to_string),
            dates: SortDates::parse(date, date, date),
            ..info(artist, title, added)
        })
    }

    // An album by one artist with a date of each kind, "" for none
    fn dated(title: &str, date: &str, original: &str, recorded: &str) -> Album {
        Album::new(AlbumInfo {
            dates: SortDates::parse(date, original, recorded),
            ..info("Artist", title, None)
        })
    }

    #[test]
    fn a_recording_copy_is_selected_on_its_own() {
        let original = album("Gidon Kremer", "Bach", Some("2005"), Some(10));
        let first = original.recording_copy("file:///m/b/01.opus");
        let second = original.recording_copy("file:///m/b/05.opus");
        assert_eq!(first.id(), original.id());
        assert_eq!(first.title(), original.title());
        assert_eq!(first.cover_path(), original.cover_path());
        // Each copy is a selection of its own
        let keys = [&original, &first, &second].map(Album::selection_key);
        assert_ne!(keys[0], keys[1]);
        assert_ne!(keys[1], keys[2]);
        assert!(keys.iter().all(|key| key.album == original.id()));
        assert_eq!(keys[0].recording, None);
        first.set_selected(true);
        assert!(!original.selected() && !second.selected());
        // The heart follows the album's
        original.set_favorite(true);
        assert!(first.favorite() && second.favorite());
    }

    fn sorted(albums: &[Album], sort: AlbumSort, descending: bool) -> Vec<String> {
        let mut albums = albums.to_vec();
        albums.sort_by(|a, b| a.compare(b, sort, descending));
        albums.iter().map(Album::title).collect()
    }

    fn albums() -> Vec<Album> {
        vec![
            album("Björk", "Post", Some("1995"), Some(300)),
            album("Air", "Talkie Walkie", Some("2004"), Some(100)),
            album("Air", "Moon Safari", Some("1998"), None),
            album("Cocteau Twins", "Untitled", None, Some(200)),
            album("Björk", "Debut", Some("1993"), Some(300)),
        ]
    }

    #[test]
    fn artist_ties_are_by_year() {
        let albums = albums();
        assert_eq!(
            sorted(&albums, AlbumSort::Artist, false),
            ["Moon Safari", "Talkie Walkie", "Debut", "Post", "Untitled"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Artist, true),
            ["Untitled", "Debut", "Post", "Moon Safari", "Talkie Walkie"]
        );
    }

    #[test]
    fn titles_in_both_directions() {
        let albums = albums();
        assert_eq!(
            sorted(&albums, AlbumSort::Title, false),
            ["Debut", "Moon Safari", "Post", "Talkie Walkie", "Untitled"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Title, true),
            ["Untitled", "Talkie Walkie", "Post", "Moon Safari", "Debut"]
        );
    }

    #[test]
    fn missing_years_are_last() {
        let albums = albums();
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::Released), true),
            ["Talkie Walkie", "Moon Safari", "Post", "Debut", "Untitled"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::Released), false),
            ["Debut", "Post", "Moon Safari", "Talkie Walkie", "Untitled"]
        );
    }

    #[test]
    fn each_date_sort_has_its_own_order() {
        let albums = [
            // Recorded in 1964, first released in 1965, and reissued
            dated("Reissue", "2015", "1965", "1964"),
            // Recorded at a concert in 1966, first released in 1998
            dated("Live", "1998", "1998", "1966"),
            dated("New", "2001", "2001", "2000"),
        ];
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::Released), false),
            ["Live", "New", "Reissue"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::OriginalRelease), false),
            ["Reissue", "Live", "New"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::Recorded), false),
            ["Reissue", "Live", "New"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::Recorded), true),
            ["New", "Live", "Reissue"]
        );
    }

    #[test]
    fn dates_without_a_month_or_day_are_last_in_their_year_or_month() {
        let albums = [
            dated("Year only", "1965", "", ""),
            dated("November", "1965-11", "", ""),
            dated("March", "1965-03", "", ""),
            dated("Next year", "1966", "", ""),
            dated("No date", "", "", ""),
            dated("March 12", "1965-03-12", "", ""),
        ];
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::Released), false),
            [
                "March 12",
                "March",
                "November",
                "Year only",
                "Next year",
                "No date"
            ]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Date(DateKind::Released), true),
            [
                "Next year",
                "November",
                "March 12",
                "March",
                "Year only",
                "No date"
            ]
        );
    }

    #[test]
    fn an_artists_albums_are_in_the_order_they_were_recorded() {
        let albums = [
            dated("Reissue", "2015", "1965", "1964"),
            dated("Autumn", "1964", "1964", "1964-10"),
            dated("Spring", "1964", "1964", "1964-04"),
        ];
        assert_eq!(
            sorted(&albums, AlbumSort::Artist, false),
            ["Spring", "Autumn", "Reissue"]
        );
    }

    #[test]
    fn added_at_the_same_time_are_from_a_to_z() {
        let albums = albums();
        assert_eq!(
            sorted(&albums, AlbumSort::Added, true),
            ["Debut", "Post", "Untitled", "Talkie Walkie", "Moon Safari"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Added, false),
            ["Talkie Walkie", "Untitled", "Debut", "Post", "Moon Safari"]
        );
    }
}
