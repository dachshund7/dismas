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
pub enum AlbumSort {
    Artist,
    Title,
    Year,
    Added,
}

impl AlbumSort {
    pub fn from_nick(nick: &str) -> Self {
        match nick {
            "title" => Self::Title,
            "year" => Self::Year,
            "added" => Self::Added,
            _ => Self::Artist,
        }
    }

    // The newest albums are the ones people look for first, while names
    // read best from A to Z
    pub fn default_descending(self) -> bool {
        matches!(self, Self::Year | Self::Added)
    }
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
    pub struct Album {
        #[property(get)]
        pub id: RefCell<String>,
        #[property(get)]
        pub title: RefCell<String>,
        #[property(get)]
        pub artist: RefCell<String>,
        #[property(get)]
        pub year: RefCell<Option<String>>,
        #[property(get)]
        pub cover_path: RefCell<Option<String>>,
        // Whether the album is selected in the grid
        #[property(get, set)]
        pub selected: Cell<bool>,
        // While a search of the grid matches only some of the tracks, the
        // number that do, and the number of all of them; otherwise 0
        #[property(get, set)]
        pub matching_tracks: Cell<u32>,
        #[property(get, set)]
        pub track_count: Cell<u32>,

        // The keys the grid is sorted by, in the order of the locale
        pub artist_key: OnceCell<glib::CollationKey>,
        pub title_key: OnceCell<glib::CollationKey>,
        pub added: Cell<Option<i64>>,
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
    pub struct Album(ObjectSubclass<imp::Album>);
}

impl Album {
    pub fn new(info: AlbumInfo) -> Self {
        let album: Self = glib::Object::new();
        let imp = album.imp();
        imp.id.replace(info.id);
        imp.title.replace(info.title);
        imp.artist.replace(info.artist);
        imp.year.replace(info.year);
        imp.cover_path.replace(info.cover);
        imp.added.set(info.added);
        let _ = imp
            .artist_key
            .set(glib::CollationKey::from(info.artist_sort.as_str()));
        let _ = imp
            .title_key
            .set(glib::CollationKey::from(imp.title.borrow().as_str()));
        album
    }

    // The order of two albums in the grid; the albums that tie are always
    // sorted from A to Z, and the oldest first
    pub fn compare(&self, other: &Self, sort: AlbumSort, descending: bool) -> Ordering {
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
        })
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
