// SPDX-FileCopyrightText: 2022  John Toohey <john_t@mailo.com>
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Filters the playlist, keeping the tracks that contain every word of the
// search, in any of their tags; matching ignores case and accents.
//
// Fuzzy matching used to be used here, but the tags of classical music are
// long enough that the letters of almost any word can be found in them, in
// order, so most of the tracks matched any search.

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::audio::normalize_search_text;

mod imp {
    use std::cell::RefCell;

    use gtk::{
        glib::{self, ParamSpec, ParamSpecString, Value},
        prelude::*,
        subclass::prelude::*,
    };
    use once_cell::sync::Lazy;

    use crate::audio::Track;

    #[derive(Default)]
    pub struct SearchFilter {
        pub search: RefCell<Option<String>>,
        // The normalized words of the search
        pub words: RefCell<Vec<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchFilter {
        const NAME: &'static str = "AmberolSearchFilter";
        type Type = super::SearchFilter;
        type ParentType = gtk::Filter;
    }

    impl ObjectImpl for SearchFilter {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> =
                Lazy::new(|| vec![ParamSpecString::builder("search").build()]);
            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &Value, pspec: &ParamSpec) {
            match pspec.name() {
                "search" => {
                    let p = value
                        .get::<Option<String>>()
                        .expect("Value must be a string");
                    self.obj().set_search(p);
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> Value {
            match pspec.name() {
                "search" => self.search.borrow().to_value(),
                _ => unimplemented!(),
            }
        }
    }

    impl FilterImpl for SearchFilter {
        fn strictness(&self) -> gtk::FilterMatch {
            if self.words.borrow().is_empty() {
                gtk::FilterMatch::All
            } else {
                gtk::FilterMatch::Some
            }
        }

        fn match_(&self, track: &glib::Object) -> bool {
            let words = self.words.borrow();
            if words.is_empty() {
                return true;
            }

            let track = track.downcast_ref::<Track>().unwrap();
            let key = track.search_key();
            words.iter().all(|word| key.contains(word.as_str()))
        }
    }
}

glib::wrapper! {
    pub struct SearchFilter(ObjectSubclass<imp::SearchFilter>)
        @extends gtk::Filter;
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchFilter {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_search(&self, search: Option<String>) {
        if *self.imp().search.borrow() == search {
            return;
        }

        let words = search
            .as_deref()
            .map(|s| {
                normalize_search_text(s)
                    .split_whitespace()
                    .map(|w| w.to_string())
                    .collect()
            })
            .unwrap_or_default();

        self.imp().search.replace(search);
        self.imp().words.replace(words);
        self.changed(gtk::FilterChange::Different);
    }
}
