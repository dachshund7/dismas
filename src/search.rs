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

    use super::*;
    use crate::audio::Track;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::SearchFilter)]
    pub struct SearchFilter {
        #[property(get, set = Self::set_search, explicit_notify, nullable)]
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

    #[glib::derived_properties]
    impl ObjectImpl for SearchFilter {}

    impl SearchFilter {
        fn set_search(&self, search: Option<String>) {
            if *self.search.borrow() == search {
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

            self.search.replace(search);
            self.words.replace(words);

            let obj = self.obj();
            obj.changed(gtk::FilterChange::Different);
            obj.notify_search();
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
}
