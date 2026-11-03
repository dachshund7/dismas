// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl LibraryView {
    // The lists of composers and of performers next to the search:
    // choosing one shows only their albums, with the tracks that are
    // theirs, like a search, and names them under the title of the page;
    // they combine with each other and with the search. All Composers or
    // All Performers, or Esc in an empty search, shows all the albums again
    pub(super) fn setup_pickers(&self) {
        let imp = self.imp();
        self.setup_picker(
            Picker {
                popover: imp.composer_popover.get(),
                search: imp.composer_search.get(),
                list: imp.composer_list.get(),
                read: library::composers,
                others: |filter| TrackFilter {
                    composer: None,
                    ..filter
                },
                search_text: |composer: &library::ComposerInfo| &composer.search,
                pick: Self::set_composer,
            },
            None,
        );
        imp.composer_list.set_factory(Some(&composer_factory()));

        // The performers are in sections: the conductors, the groups and
        // the players, with All Performers first, in a section of its own
        let sections = gtk::CustomSorter::new(|a, b| {
            let kind = |item: &glib::Object| {
                let item = item.downcast_ref::<glib::BoxedAnyObject>().unwrap();
                let performer = item.borrow::<Option<library::PerformerInfo>>();
                performer.as_ref().map(|performer| performer.kind)
            };
            kind(a).cmp(&kind(b)).into()
        });
        self.setup_picker(
            Picker {
                popover: imp.performer_popover.get(),
                search: imp.performer_search.get(),
                list: imp.performer_list.get(),
                read: library::performers,
                others: |filter| TrackFilter {
                    performer: None,
                    ..filter
                },
                search_text: |performer: &library::PerformerInfo| &performer.search,
                pick: Self::set_performer,
            },
            Some(sections.upcast()),
        );
        imp.performer_list.set_factory(Some(&performer_factory()));
        imp.performer_list
            .set_header_factory(Some(&performer_section_factory()));
    }

    // Sets up a list to pick from: it's read each time it opens, as the
    // albums can change, with None, for all of them, first. It only has the
    // items with a track that the search and the other picks match, so that
    // a composer's list of performers has only theirs, and the other way
    // round, and its counts are of those albums; on the Works page, only
    // the tracks of works count, and the counts are of works. Its search
    // finds the items by their `search_text`, and Enter takes the first one
    // found. The `sections` sorter, if any, divides the list into sections,
    // which the list is already sorted by
    fn setup_picker<T: Clone + Send + 'static>(
        &self,
        picker: Picker<T>,
        sections: Option<gtk::Sorter>,
    ) {
        let Picker {
            popover,
            search,
            list,
            read,
            others,
            search_text,
            pick,
        } = picker;
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        // The words searched for, normalized once for all the items
        let words: Rc<RefCell<Vec<String>>> = Rc::default();
        let filter = gtk::CustomFilter::new(clone!(
            #[strong]
            words,
            move |item| {
                let words = words.borrow();
                let item = item.downcast_ref::<glib::BoxedAnyObject>().unwrap();
                match item.borrow::<Option<T>>().as_ref() {
                    // "All Composers" isn't something to search for
                    None => words.is_empty(),
                    Some(item) => {
                        let text = search_text(item);
                        words.iter().all(|word| text.contains(word.as_str()))
                    }
                }
            }
        ));
        let model: gio::ListModel = match sections {
            Some(sections) => {
                let sorted = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
                sorted.set_section_sorter(Some(&sections));
                sorted.upcast()
            }
            None => store.clone().upcast(),
        };
        let filtered = gtk::FilterListModel::new(Some(model), Some(filter.clone()));
        list.set_model(Some(&gtk::NoSelection::new(Some(filtered.clone()))));

        search.connect_search_changed(clone!(
            #[weak]
            filter,
            move |entry| {
                words.replace(
                    normalize_search_text(&entry.text())
                        .split_whitespace()
                        .map(ToString::to_string)
                        .collect(),
                );
                filter.changed(gtk::FilterChange::Different);
            }
        ));
        search.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            popover,
            #[weak]
            filtered,
            move |_| this.pick_from(&popover, &filtered, 0, pick)
        ));
        list.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            popover,
            #[weak]
            filtered,
            move |_, pos| this.pick_from(&popover, &filtered, pos, pick)
        ));

        popover.connect_show(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            store,
            #[weak]
            search,
            #[weak]
            list,
            move |_| {
                search.set_text("");
                search.grab_focus();
                let hidden = this.imp().hidden_folders.borrow().clone();
                let filter = others(this.wanted_filter());
                let counted = if this.shows_works() {
                    Counted::Works
                } else {
                    Counted::Albums
                };
                glib::spawn_future_local(clone!(
                    #[weak]
                    store,
                    #[weak]
                    list,
                    async move {
                        let items = gio::spawn_blocking(move || read(&hidden, &filter, counted))
                            .await
                            .unwrap_or_default();
                        let items: Vec<glib::BoxedAnyObject> = std::iter::once(None)
                            .chain(items.into_iter().map(Some))
                            .map(glib::BoxedAnyObject::new)
                            .collect();
                        store.splice(0, store.n_items(), &items);
                        // From the top, with All Composers, rather than
                        // where it was left, at the one picked. The search
                        // can have found none, if it was typed in while
                        // they were read
                        if list.model().is_some_and(|model| model.n_items() > 0) {
                            list.scroll_to(0, gtk::ListScrollFlags::NONE, None);
                        }
                    }
                ));
            }
        ));
    }

    fn pick_from<T: Clone + 'static>(
        &self,
        popover: &gtk::Popover,
        list: &gtk::FilterListModel,
        pos: u32,
        pick: fn(&Self, Option<T>),
    ) {
        let Some(item) = list.item(pos).and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let picked = item.borrow::<Option<T>>().clone();
        popover.popdown();
        pick(self, picked);
        // The popover gives the focus back to its button as it closes. The
        // search takes it, as the albums picked are searched further there,
        // and Esc in it shows all the albums again
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = this)]
            self,
            move || this.focus_search()
        ));
    }

    // Shows only the albums of a composer, or of all of them
    fn set_composer(&self, composer: Option<library::ComposerInfo>) {
        self.imp().composer.replace(composer);
        self.update_picks();
        self.update_search();
    }

    // Shows only the albums of a performer, or of all of them
    fn set_performer(&self, performer: Option<library::PerformerInfo>) {
        self.imp().performer.replace(performer);
        self.update_picks();
        self.update_search();
    }

    // The Composers and Performers buttons are only there while a track
    // has a composer, or a performer. One picked goes with its button, so
    // that the albums aren't left filtered by a button that can't be seen
    pub(super) fn set_pickers_available(&self, composers: bool, performers: bool) {
        let imp = self.imp();
        imp.composer_button.set_visible(composers);
        imp.performer_button.set_visible(performers);
        let composer = !composers && imp.composer.replace(None).is_some();
        let performer = !performers && imp.performer.replace(None).is_some();
        if composer || performer {
            self.update_picks();
            self.update_search();
        }
    }

    // Whether only the albums of a composer or a performer, or only the
    // favorites, are shown
    pub(crate) fn has_picks(&self) -> bool {
        self.has_composer_or_performer() || self.imp().favorites_button.is_active()
    }

    pub(super) fn has_composer_or_performer(&self) -> bool {
        let imp = self.imp();
        imp.composer.borrow().is_some() || imp.performer.borrow().is_some()
    }

    // Shows the albums of all the composers and performers again, and not
    // only the favorites
    pub(crate) fn clear_picks(&self) {
        let imp = self.imp();
        imp.composer.replace(None);
        imp.performer.replace(None);
        self.update_picks();
        if imp.favorites_button.is_active() {
            // Which searches again
            imp.favorites_button.set_active(false);
        } else {
            self.update_search();
        }
    }

    // The composer and the performer whose albums are shown are under the
    // title of the page, and on the tooltips of their buttons, as a narrow
    // window leaves out the title; the buttons are highlighted
    pub(super) fn update_picks(&self) {
        let imp = self.imp();
        let composer = imp.composer.borrow();
        let performer = imp.performer.borrow();
        // Once, if they're the same, as for a composer who plays
        let names = [
            composer.as_ref().map(|c| c.name.as_str()),
            performer
                .as_ref()
                .filter(|p| composer.as_ref().is_none_or(|c| c.key != p.key))
                .map(|p| p.name.as_str()),
        ];
        imp.page_title
            .set_subtitle(&names.into_iter().flatten().collect::<Vec<_>>().join(" · "));

        imp.composer_button
            .set_tooltip_text(Some(&match composer.as_ref() {
                Some(composer) => i18n_k(
                    // Translators: `{composer}` must be left unmodified; it is
                    // expanded to the name of the composer whose albums are
                    // shown
                    "Composers: {composer}",
                    &[("composer", &composer.name)],
                ),
                None => i18n("Composers"),
            }));
        imp.performer_button
            .set_tooltip_text(Some(&match performer.as_ref() {
                Some(performer) => i18n_k(
                    // Translators: `{performer}` must be left unmodified; it is
                    // expanded to the name of the conductor, the group or the
                    // player whose albums are shown
                    "Performers: {performer}",
                    &[("performer", &performer.name)],
                ),
                None => i18n("Performers"),
            }));
        for (button, picked) in [
            (&imp.composer_button, composer.is_some()),
            (&imp.performer_button, performer.is_some()),
        ] {
            if picked {
                button.add_css_class("filtering");
            } else {
                button.remove_css_class("filtering");
            }
        }
    }
}

// A list next to the search, to pick a composer or a performer from, see
// `LibraryView::setup_picker()`
struct Picker<T> {
    popover: gtk::Popover,
    search: gtk::SearchEntry,
    list: gtk::ListView,
    // Reads the items, in another thread, without the hidden folders,
    // those of the tracks the filter matches, with the number of albums,
    // or of works on the Works page
    read: fn(&[PathBuf], &TrackFilter, Counted) -> Vec<T>,
    // The filter of the items that go with the others picked and the
    // search: the one of the albums shown, without the item picked
    others: fn(TrackFilter) -> TrackFilter,
    // The text, normalized, that the list's search looks into
    search_text: fn(&T) -> &str,
    // Shows the albums of the item picked, or of all of them
    pick: fn(&LibraryView, Option<T>),
}

// The line of a row of the lists of composers, performers and works: a
// name, which the end of a long row cuts off, and a number. The scrollbar
// is drawn over the right edge of the list. The line is as high as the
// theme makes a row at least, so that its text is in the middle of the row
// rather than at its top, and the line of an open work, with its covers
// below it, is as high (style.css)
pub(super) fn fill_name_count_line(line: &gtk::Box, name: &gtk::Label, count: &gtk::Label) {
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    count.add_css_class("dim-label");
    count.add_css_class("numeric");
    line.set_spacing(12);
    line.set_margin_end(12);
    line.set_vexpand(true);
    line.add_css_class("row-line");
    line.append(name);
    line.append(count);
}

// The rows of the lists of composers and performers: a line with a name
// and a number, which `bind` sets from the `T` of the item, a
// `BoxedAnyObject`; `tooltip` makes the line's tooltip from it when it
// shows, and its name for screen readers, with the number, and
// `description` what they say about the row after its name. Their line is
// in a box, as a work's is in its row
fn name_count_factory<T: 'static>(
    tooltip: fn(&T) -> Option<String>,
    description: fn(&T) -> Option<String>,
    bind: fn(&T, &gtk::Label, &gtk::Label),
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (line, name, count) = Default::default();
        fill_name_count_line(&line, &name, &count);
        utils::set_row_tooltip(&line, item, move |object| {
            tooltip(&object.downcast_ref::<glib::BoxedAnyObject>()?.borrow::<T>())
        });
        // Set, so that the line's expanding doesn't spread to the list
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(false)
            .build();
        row.append(&line);
        item.set_child(Some(&row));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(line) = item.child().and_then(|row| row.first_child()) else {
            return;
        };
        let (Some(name), Some(count)) = (
            line.first_child().and_downcast::<gtk::Label>(),
            line.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        if let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() {
            let object = object.borrow::<T>();
            bind(&object, &name, &count);
            // A row has no name of its own for screen readers: the one
            // it's known by, with its roles, as in its tooltip, or the
            // label of All …, and its number
            let label = tooltip(&object).map_or_else(
                || name.text().to_string(),
                |tooltip| tooltip.replace('\n', ", "),
            );
            let count = count.text();
            let label = if count.is_empty() {
                label
            } else {
                format!("{label}, {count}")
            };
            item.set_accessible_label(&label);
            item.set_accessible_description(&description(&object).unwrap_or_default());
        }
    });
    factory
}

// A name followed, dimmed, by more about it, if there's more, as markup
pub(super) fn with_dimmed(name: &str, more: &str) -> String {
    let name = glib::markup_escape_text(name);
    if more.is_empty() {
        name.to_string()
    } else {
        let more = glib::markup_escape_text(more);
        format!("{name}  <span alpha=\"55%\">{more}</span>")
    }
}

// The rows of the list of composers: the name they're sorted by, and the
// number of their albums
fn composer_factory() -> gtk::SignalListItemFactory {
    name_count_factory::<Option<library::ComposerInfo>>(
        // The name it's known by, as the row has the one it's sorted by
        |composer| composer.as_ref().map(|composer| composer.name.clone()),
        |_| None,
        |composer, name, count| {
            if let Some(composer) = composer {
                name.set_label(&composer.label);
                count.set_label(&composer.count.to_string());
            } else {
                name.set_label(&i18n("All Composers"));
                count.set_label("");
            }
        },
    )
}

// The rows of the list of performers: the name they're sorted by, then,
// dimmed, what they play or sing, which the end of a long row cuts off
// first, and the number of their albums
fn performer_factory() -> gtk::SignalListItemFactory {
    name_count_factory::<Option<library::PerformerInfo>>(
        // The name they're known by, and all they play or sing
        |performer| {
            let performer = performer.as_ref()?;
            let roles = performer.roles.join(", ");
            let text = [performer.name.as_str(), roles.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            Some(text)
        },
        // The section they're in
        |performer| {
            performer
                .as_ref()
                .map(|performer| section_name(performer.kind))
        },
        |performer, name, count| {
            if let Some(performer) = performer {
                name.set_markup(&with_dimmed(&performer.sort, &performer.roles.join(", ")));
                count.set_label(&performer.count.to_string());
            } else {
                name.set_label(&i18n("All Performers"));
                count.set_label("");
            }
        },
    )
}

// The headers of the sections of the list of performers: a line, then the
// name of the section, in bold, so that the sections stand out from the
// dimmed roles in the rows. All Performers, at the top, has neither. The
// section that starts the list has no line either, as when a search hides
// All Performers, which leaves a line under the entry otherwise
fn performer_section_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
        let separator = gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(["picker-separator"])
            .build();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading", "picker-section"])
            .build();
        let child = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        child.append(&separator);
        child.append(&label);
        header.set_child(Some(&child));
        header.connect_start_notify(update_section_line);
    });
    factory.connect_bind(|_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
        let Some(label) = header
            .child()
            .and_then(|child| child.last_child())
            .and_downcast::<gtk::Label>()
        else {
            return;
        };
        let kind = header
            .item()
            .and_downcast::<glib::BoxedAnyObject>()
            .and_then(|item| {
                item.borrow::<Option<library::PerformerInfo>>()
                    .as_ref()
                    .map(|performer| performer.kind)
            });
        label.set_visible(kind.is_some());
        label.set_label(&kind.map(section_name).unwrap_or_default());
        update_section_line(header);
    });
    factory
}

// Shows the line above a section's header unless the section has no name
// or starts the list
fn update_section_line(header: &gtk::ListHeader) {
    let Some(child) = header.child() else {
        return;
    };
    let (Some(separator), Some(label)) = (child.first_child(), child.last_child()) else {
        return;
    };
    separator.set_visible(label.is_visible() && header.start() > 0);
}

// The name of the section of the performers of a kind, its header, which
// the rows also give screen readers, as they skip the headers
fn section_name(kind: PerformerKind) -> String {
    match kind {
        PerformerKind::Conductor => i18n("Conductors"),
        PerformerKind::Ensemble => i18n("Ensembles"),
        PerformerKind::Soloist => i18n("Soloists"),
        PerformerKind::Artist => i18n("Artists"),
    }
}
