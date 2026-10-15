// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::AlbumInfo;

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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Album {
        const NAME: &'static str = "AmberolAlbum";
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
        album
    }
}
