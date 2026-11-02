// SPDX-FileCopyrightText: 2022  John Toohey <john_t@mailo.com>
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Filters the queue with a search in the search syntax (`search_query`):
// plain words are found in any of the tags, ignoring case and accents, and
// fields, OR, "-" and the rest narrow it down. Also shows what's wrong with
// a search under its entry, for the album search too.
//
// Fuzzy matching used to be used here, but the tags of classical music are
// long enough that the letters of almost any word can be found in them, in
// order, so most of the tracks matched any search.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use gtk::{glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::{
    i18n::i18n,
    library::{self, TrackEntry},
    play_stats::{self, SearchStats},
    search_query::{Context, Mode, Needs, Problem, Query},
};

// What a search of the queue needs that its tracks don't have, read when a
// search needs it, see `Needs`
#[derive(Debug, Default)]
pub(crate) struct QueueSources {
    // From plays.db, by URI
    pub(crate) stats: SearchStats,
    // From the library, by path
    pub(crate) tracks: HashMap<PathBuf, TrackEntry>,
    // What they were read for, and plays.db's `writes()` then
    needs: Needs,
    writes: u64,
}

impl QueueSources {
    fn read(needs: &Needs) -> Self {
        Self {
            writes: play_stats::writes(),
            stats: play_stats::search_stats(needs),
            tracks: if needs.library_entry() {
                library::track_entries()
            } else {
                HashMap::new()
            },
            needs: *needs,
        }
    }

    // Whether they have what a search that needs `needs` reads, as they
    // were read, with plays.db at `writes`: each key typed would otherwise
    // read all the tracks of the library again, and all their play stats,
    // on the main thread
    fn cover(&self, needs: &Needs, writes: u64) -> bool {
        let read = &self.needs;
        (!needs.library_entry() || read.library_entry())
            && (!needs.stats || read.stats)
            && (!needs.favorites || read.favorites)
            && (!needs.favorite_albums || read.favorite_albums)
            && (!needs.favorite_works || read.favorite_works)
            && self.writes == writes
    }
}

mod imp {
    use super::*;
    use crate::audio::Track;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::SearchFilter)]
    pub(crate) struct SearchFilter {
        #[property(get, set = Self::set_search, explicit_notify, nullable)]
        pub(crate) search: RefCell<Option<String>>,
        // The search, unless it matches every track, as an empty one does;
        // without one, the search doesn't filter the queue
        #[property(name = "filtering", get = Self::filtering, type = bool)]
        pub(crate) query: RefCell<Option<Query>>,
        // When the search was typed, which its relative dates were worked
        // out at, see `Query::at()`
        pub(crate) context: Cell<Option<Context>>,
        // What the search needs from the library and plays.db, read when
        // it's typed, and again when a favorite is marked
        // (`favorites_changed()`)
        pub(crate) sources: RefCell<Option<QueueSources>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchFilter {
        const NAME: &'static str = "DismasSearchFilter";
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

            let context = Context::now();
            let query = search
                .as_deref()
                .map(|text| Query::parse(text, Mode::Search))
                .filter(|query| !query.matches_all())
                .map(|query| query.at(&context));
            let sources = query.as_ref().map(|query| {
                let needs = query.needs();
                match self.sources.take() {
                    Some(sources) if sources.cover(&needs, play_stats::writes()) => sources,
                    _ => QueueSources::read(&needs),
                }
            });
            self.sources.replace(sources);

            let was_filtering = self.filtering();
            self.search.replace(search);
            self.query.replace(query);
            self.context.set(Some(context));

            let obj = self.obj();
            obj.changed(gtk::FilterChange::Different);
            obj.notify_search();
            if self.filtering() != was_filtering {
                obj.notify_filtering();
            }
        }

        fn filtering(&self) -> bool {
            self.query.borrow().is_some()
        }
    }

    impl FilterImpl for SearchFilter {
        fn strictness(&self) -> gtk::FilterMatch {
            match &*self.query.borrow() {
                None => gtk::FilterMatch::All,
                Some(query) if query.has_errors() => gtk::FilterMatch::None,
                Some(_) => gtk::FilterMatch::Some,
            }
        }

        fn match_(&self, track: &glib::Object) -> bool {
            let query = self.query.borrow();
            let Some(query) = &*query else {
                return true;
            };
            let context = self.context.get().unwrap_or_else(Context::now);
            let track = track.downcast_ref::<Track>().unwrap();
            let sources = self.sources.borrow();
            track.matches_query(query, &context, sources.as_ref())
        }
    }
}

glib::wrapper! {
    pub(crate) struct SearchFilter(ObjectSubclass<imp::SearchFilter>)
        @extends gtk::Filter;
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchFilter {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    // A track, an album or a work was marked a favorite, or unmarked; a
    // search for the favorites finds it, or leaves it out, at once
    pub(crate) fn favorites_changed(&self) {
        let imp = self.imp();
        let needs = imp.query.borrow().as_ref().map(Query::needs);
        if !needs.as_ref().is_some_and(Needs::any_favorites) {
            return;
        }
        // What the library keeps of the tracks stays the same
        if let Some(sources) = imp.sources.borrow_mut().as_mut() {
            sources.writes = play_stats::writes();
            sources.stats = play_stats::search_stats(&sources.needs);
        }
        self.changed(gtk::FilterChange::Different);
    }
}

// How long a problem waits after the search changes before it shows, so
// that its line doesn't come and go with each key typed
const PROBLEM_DELAY: Duration = Duration::from_millis(500);

// Shows what's wrong with the search of an entry, or what to know about
// it. While it has an error, the entry is red, and invalid for screen
// readers; the error, or else a hint, shows a moment after typing stops,
// through `show` (`None` hides it, at once), and is announced once. The
// entry's description, `description`, is what screen readers say it's for,
// and its tooltip says what can be typed
pub(crate) fn setup_query_entry(
    entry: &gtk::SearchEntry,
    description: String,
    show: impl Fn(Option<&str>) + 'static,
) {
    let show = Rc::new(show);
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let announced: Rc<RefCell<Option<String>>> = Rc::default();

    entry.set_has_tooltip(true);
    entry.connect_query_tooltip(|_, _, _, _, tooltip| {
        tooltip.set_text(Some(&i18n(
            "Search in the tags, or in a field, e.g. composer:bach year:<1970; Search Syntax in the main menu has them all",
        )));
        true
    });
    entry.update_property(&[gtk::accessible::Property::Description(&description)]);

    entry.connect_search_changed(clone!(
        #[strong]
        show,
        move |entry| {
            let query = Query::parse(&entry.text(), Mode::Search);
            let problems = query.problems();
            let problem = problems
                .iter()
                .find(|problem| problem.is_error(Mode::Search))
                .or_else(|| problems.first())
                .map(Problem::message);

            if query.has_errors() {
                entry.add_css_class("error");
                entry.update_state(&[gtk::accessible::State::Invalid(
                    gtk::AccessibleInvalidState::True,
                )]);
            } else {
                entry.remove_css_class("error");
                entry.reset_state(gtk::AccessibleState::Invalid);
            }
            let full = match &problem {
                Some(problem) => format!("{description} {problem}"),
                None => description.clone(),
            };
            entry.update_property(&[gtk::accessible::Property::Description(&full)]);

            if let Some(id) = pending.take() {
                id.remove();
            }
            let Some(problem) = problem else {
                show(None);
                announced.replace(None);
                return;
            };
            let id = glib::timeout_add_local_once(
                PROBLEM_DELAY,
                clone!(
                    #[weak]
                    entry,
                    #[strong]
                    show,
                    #[strong]
                    pending,
                    #[strong]
                    announced,
                    move || {
                        pending.replace(None);
                        show(Some(&problem));
                        // Changes to a description aren't said
                        if announced.borrow().as_deref() != Some(problem.as_str()) {
                            entry.announce(&problem, gtk::AccessibleAnnouncementPriority::Medium);
                            announced.replace(Some(problem));
                        }
                    }
                ),
            );
            pending.replace(Some(id));
        }
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sources_read_serve_the_searches_that_need_as_much() {
        let read = Needs {
            added: true,
            stats: true,
            ..Needs::default()
        };
        let sources = QueueSources {
            needs: read,
            writes: 7,
            ..QueueSources::default()
        };
        assert!(sources.cover(&read, 7));
        // Less, or the same with words to look for too
        assert!(sources.cover(&Needs::default(), 7));
        assert!(sources.cover(
            &Needs {
                search_key: true,
                ..read
            },
            7
        ));
        // More
        assert!(!sources.cover(
            &Needs {
                favorites: true,
                ..read
            },
            7
        ));
        // Once plays.db was written to
        assert!(!sources.cover(&read, 8));
    }
}
