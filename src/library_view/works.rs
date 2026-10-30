// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The Works page: the list of the works the search finds, under the names
// of their composers, which takes the place of the grid

use super::*;

impl LibraryView {
    // The list of the works the search finds, under the names of their
    // composers, which takes the place of the grid on the Works page;
    // activating one shows its recordings
    pub(super) fn setup_works(&self) {
        let imp = self.imp();
        imp.works_list.set_factory(Some(&work_factory(clone!(
            #[weak(rename_to = this)]
            self,
            move |widget: &gtk::Widget, x, y, work: &library::WorkInfo| {
                let item =
                    glib::BoxedAnyObject::new((work.clone(), this.search_filter_for_works()));
                this.emit_by_name::<()>("work-menu-requested", &[&item, widget, &x, &y]);
            }
        ))));
        // The rows' menus open with the Menu key too, through their action
        utils::setup_menu_key(&*imp.works_list, |row: &gtk::Box| {
            let _ = row.activate_action("work-row.menu", None);
        });
        // Showing only the favorite works reads them again
        imp.favorites_button.connect_toggled(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.refresh_favorites()
        ));
        imp.works_list
            .set_header_factory(Some(&work_section_factory()));
        self.set_works_model();

        imp.works_list.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            move |list, pos| {
                let Some(item) = list
                    .model()
                    .and_then(|model| model.item(pos))
                    .and_downcast::<glib::BoxedAnyObject>()
                else {
                    return;
                };
                let work = item.borrow::<library::WorkInfo>().clone();
                let filter = this.search_filter_for_works();
                let item = glib::BoxedAnyObject::new((work, filter));
                this.emit_by_name::<()>("work-activated", &[&item]);
            }
        ));
    }

    // Gives the list of works a new model, which it shows from the top, with
    // a section for each composer, or composers, as the works come sorted
    // by them
    fn set_works_model(&self) -> gio::ListStore {
        let imp = self.imp();
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sections = WorkSectionModel::new(&store);
        sections.set_same_section(Some(same_section_of(
            |previous: &glib::BoxedAnyObject, work: &glib::BoxedAnyObject| {
                let previous = previous.borrow::<library::WorkInfo>();
                let work = work.borrow::<library::WorkInfo>();
                previous.composer_keys().eq(work.composer_keys())
            },
        )));
        imp.works_list
            .set_model(Some(&gtk::NoSelection::new(Some(sections))));
        imp.works.replace(Some(store.clone()));
        store
    }

    // The filter the works shown were read with, which their recordings
    // are found with
    fn search_filter_for_works(&self) -> TrackFilter {
        self.imp()
            .works_filter
            .borrow()
            .as_ref()
            .map(|(filter, _)| filter.clone())
            .unwrap_or_default()
    }

    // Reads the works the search finds again, if they're shown and it
    // changed since they were read, or the library was read again
    pub(super) fn update_works(&self) {
        let imp = self.imp();
        if !self.shows_works() {
            return;
        }
        let wanted = (self.wanted_filter(), imp.works_generation.get());
        if imp.works_filter.borrow().as_ref() == Some(&wanted) {
            return;
        }

        let hidden = imp.hidden_folders.borrow().clone();
        let favorites_only = imp.favorites_button.is_active();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let (filter, generation) = wanted;
                let Ok((filter, works)) = gio::spawn_blocking(move || {
                    let favorites = play_stats::favorite_work_keys();
                    let mut works = library::works(&filter, &hidden);
                    for work in &mut works {
                        work.favorite = favorites.contains(&work.key);
                    }
                    if favorites_only {
                        works.retain(|work| work.favorite);
                    }
                    (filter, works)
                })
                .await
                else {
                    return;
                };
                // Results for an older search, or from before the library
                // was read again, arriving late
                if this.wanted_filter() != filter
                    || this.imp().works_generation.get() != generation
                    || !this.shows_works()
                {
                    return;
                }
                this.show_works(filter, generation, works);
            }
        ));
    }

    // Reads the works again, after a work or a recording was marked as a
    // favorite or unmarked, or the list was set to show only the favorites
    pub fn refresh_favorites(&self) {
        let generation = &self.imp().works_generation;
        generation.set(generation.get().wrapping_add(1));
        self.update_works();
    }

    // A work's recordings are found with the filter, as albums are, but
    // they're shown and played whole: a recording is one piece of music,
    // and a soloist found in one movement plays in the others too
    fn show_works(&self, filter: TrackFilter, generation: u32, works: Vec<library::WorkInfo>) {
        let imp = self.imp();
        let new_search = imp
            .works_filter
            .borrow()
            .as_ref()
            .is_none_or(|(before, _)| *before != filter);
        imp.works_filter.replace(Some((filter, generation)));

        // A new search shows what it found from the top, with the header of
        // the first work. The list keeps its place by an item, so it's given
        // a new model, which it shows from the first work, and then it's
        // scrolled to the top
        let store = if new_search {
            let store = self.set_works_model();
            utils::scroll_to_top_when_laid_out(&imp.works_list, || true);
            store
        } else {
            let Some(store) = imp.works.borrow().clone() else {
                return;
            };
            store
        };
        // When the works are read again because the library was, only the
        // ones that changed are replaced, so the list stays where it was,
        // and stays as it was when none changed
        let shown = |pos: usize| {
            store
                .item(pos as u32)
                .and_downcast::<glib::BoxedAnyObject>()
        };
        let (start, removed, added) =
            utils::changed_range(store.n_items() as usize, works.len(), |old, new| {
                shown(old).is_some_and(|work| *work.borrow::<library::WorkInfo>() == works[new])
            });
        let added: Vec<glib::BoxedAnyObject> = works
            .into_iter()
            .skip(start)
            .take(added)
            .map(glib::BoxedAnyObject::new)
            .collect();
        store.splice(start as u32, removed as u32, &added);
    }
}

// The number of the recordings of a work, e.g. "3 recordings"
pub fn recordings_label(n: u32) -> String {
    ni18n_f(
        // Translators: The '{}' must be left unmodified, and it is
        // expanded to the number of recordings of a work in the music
        // folders
        "{} recording",
        "{} recordings",
        n,
        &[&n.to_string()],
    )
}

// The rows of the list of works: the name of the work, then, dimmed, the
// work or the collection it's part of, if any, which the end of a long row
// cuts off first, and the number of its recordings. The rows are all one
// line high, as measuring wrapped text made scrolling slow; the tooltip has
// the full title
fn work_factory(
    on_menu: impl Fn(&gtk::Widget, f64, f64, &library::WorkInfo) + 'static,
) -> gtk::SignalListItemFactory {
    let factory = name_count_factory::<library::WorkInfo>(
        |work| {
            let recordings = recordings_label(work.recordings);
            let arrangement = work.arrangement.as_ref().map(|arrangement| {
                i18n_k(
                    // Translators: `{work}` must be left unmodified; it is
                    // expanded to the title of the work an arrangement
                    // arranges
                    "Arrangement of {work}",
                    &[("work", &arrangement.of)],
                )
            });
            let text = [
                Some(work.title.as_deref().unwrap_or(&work.name)),
                arrangement.as_deref(),
                work.collection.as_deref(),
                Some(&recordings),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
            Some(text)
        },
        |work, name, count| {
            // What an arrangement is, e.g. "for two pianos, Busoni", tells
            // it from the work it arranges, whose title it often has
            let arrangement = work.arrangement.as_ref().map(Arrangement::shown);
            let more = [arrangement.as_deref(), work.collection.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            name.set_markup(&with_dimmed(&work.name, &more));
            count.set_label(&work.recordings.to_string());
        },
    );

    // A heart before the number of recordings of a favorite work, and the
    // row's menu, with a right click or a long press, and with the Menu key
    // through the row's action, as keys go to the list
    let on_menu = Rc::new(on_menu);
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let heart = gtk::Image::builder()
            .icon_name("heart-filled-symbolic")
            .css_classes(["favorite-work"])
            .visible(false)
            .build();
        heart.update_property(&[gtk::accessible::Property::Label(&i18n("Favorite"))]);
        if let Some(name) = row.first_child() {
            row.insert_child_after(&heart, Some(&name));
        }

        let open = Rc::new(clone!(
            #[weak]
            item,
            #[strong]
            on_menu,
            move |widget: &gtk::Widget, x: f64, y: f64| {
                if let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() {
                    on_menu(widget, x, y, &object.borrow::<library::WorkInfo>());
                }
            }
        ));
        utils::setup_context_menu(
            &row,
            clone!(
                #[weak]
                row,
                #[strong]
                open,
                move |x, y| open(row.upcast_ref(), x, y)
            ),
        );
        let menu = gio::SimpleAction::new("menu", None);
        menu.connect_activate(clone!(
            #[weak]
            row,
            move |_, _| {
                let (x, y) = (f64::from(row.width()) / 2.0, f64::from(row.height()) / 2.0);
                open(row.upcast_ref(), x, y);
            }
        ));
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&menu);
        row.insert_action_group("work-row", Some(&actions));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let heart = item
            .child()
            .and_then(|row| row.first_child())
            .and_then(|name| name.next_sibling())
            .and_downcast::<gtk::Image>();
        let work = item.item().and_downcast::<glib::BoxedAnyObject>();
        if let (Some(heart), Some(work)) = (heart, work) {
            heart.set_visible(work.borrow::<library::WorkInfo>().favorite);
        }
    });
    factory
}

// The headers of the list of works: the names of the composers
fn work_section_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading", "works-composer"])
            .build();
        header.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
        let Some(label) = header.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(work) = header.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let names = work.borrow::<library::WorkInfo>().composer_names();
        label.set_label(&if names.is_empty() {
            i18n("Unknown composer")
        } else {
            names
        });
    });
    factory
}
