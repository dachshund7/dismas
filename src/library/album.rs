// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    cmp::Ordering,
};

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::AlbumInfo;

// What the album grid is sorted by, as in the "album-sort" setting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AlbumSort {
    Artist,
    Title,
    Year,
    Added,
}

impl AlbumSort {
    pub(crate) fn from_nick(nick: &str) -> Self {
        match nick {
            "title" => Self::Title,
            "year" => Self::Year,
            "added" => Self::Added,
            _ => Self::Artist,
        }
    }

    // The newest albums are the ones people look for first, while names
    // read best from A to Z
    pub(crate) fn default_descending(self) -> bool {
        matches!(self, Self::Year | Self::Added)
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
        let year = |descending| {
            cmp_present_first(
                a.year.borrow().as_ref(),
                b.year.borrow().as_ref(),
                descending,
            )
        };
        let added =
            || cmp_present_first(a.added.get().as_ref(), b.added.get().as_ref(), descending);

        match sort {
            AlbumSort::Artist => artist(descending)
                .then_with(|| year(false))
                .then_with(|| title(false)),
            AlbumSort::Title => title(descending)
                .then_with(|| artist(false))
                .then_with(|| year(false)),
            AlbumSort::Year => year(descending)
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

    fn album(artist: &str, title: &str, year: Option<&str>, added: Option<i64>) -> Album {
        Album::new(AlbumInfo {
            id: format!("{artist} {title}"),
            title: title.to_string(),
            artist: artist.to_string(),
            artist_sort: artist.to_string(),
            year: year.map(str::to_string),
            added,
            cover: None,
            label: None,
            catalog_number: None,
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
            sorted(&albums, AlbumSort::Year, true),
            ["Talkie Walkie", "Moon Safari", "Post", "Debut", "Untitled"]
        );
        assert_eq!(
            sorted(&albums, AlbumSort::Year, false),
            ["Debut", "Post", "Moon Safari", "Talkie Walkie", "Untitled"]
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
