// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The Works page: the list of the works the search finds, under the names
// of their composers, which takes the place of the grid. A work opens, one
// at a time, to show the covers of the albums it's recorded on, which take
// the work's tracks from them, as the Tracks page takes those a search
// matches

use super::*;
use crate::{
    album_cell::CellWork, play_stats::FavoriteRecording, queue_row, reveal_bin::RevealBin,
};

// The covers of the albums of an open work, smaller than the grid's, as
// they're in a list
const WORK_COVER_MIN: i32 = 120;
const WORK_COVER_MAX: i32 = 160;

// The work of the list that's open, with the covers of its albums
#[derive(Debug)]
pub struct ExpandedWork {
    key: String,
    name: String,
    covers: Vec<WorkCover>,
}

// The cover of an album a work is recorded on: how many of the album's
// tracks are the work's, and how many it has, who performs it there and
// when, and its recordings there, which can be favorites
#[derive(Debug)]
struct WorkCover {
    album: Album,
    matching: u32,
    total: u32,
    subtitle: String,
    recordings: Vec<FavoriteRecording>,
    favorite: bool,
}

impl LibraryView {
    // The list of the works the search finds, under the names of their
    // composers, which takes the place of the grid on the Works page;
    // activating one opens or closes it
    pub(super) fn setup_works(&self) {
        let imp = self.imp();
        let factory = work_factory(clone!(
            #[weak(rename_to = this)]
            self,
            move |widget: &gtk::Widget, x, y, work: &library::WorkInfo| {
                let item = glib::BoxedAnyObject::new(work.clone());
                this.emit_by_name::<()>("work-menu-requested", &[&item, widget, &x, &y]);
            }
        ));
        self.setup_work_rows(&factory);
        imp.works_list.set_factory(Some(&factory));
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
                this.toggle_work(&work);
            }
        ));
    }

    // The row of each work keeps the key of its work, to be found when the
    // work opens or closes, and is open while its work is
    fn setup_work_rows(&self, factory: &gtk::SignalListItemFactory) {
        // A row is bound before the list takes its item's widget, which
        // tells screen readers whether its work is open, so the widget
        // only gets it once the row is on screen
        factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(row) = item.child() else {
                    return;
                };
                row.connect_map(clone!(
                    #[weak]
                    this,
                    #[weak]
                    item,
                    move |row| {
                        if let Some(key) = work_key(&item) {
                            this.update_item_state(row, &key);
                        }
                    }
                ));
            }
        ));
        factory.connect_bind(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let (Some(row), Some(key)) =
                    (item.child().and_downcast::<gtk::Box>(), work_key(item))
                else {
                    return;
                };
                let rows = &this.imp().work_rows;
                rows.borrow_mut().insert(key.clone(), row.downgrade());
                this.update_work_row(&row, &key, false);
            }
        ));
        factory.connect_unbind(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let (Some(row), Some(key)) =
                    (item.child().and_downcast::<gtk::Box>(), work_key(item))
                else {
                    return;
                };
                let mut rows = this.imp().work_rows.borrow_mut();
                if rows.get(&key).and_then(glib::WeakRef::upgrade) == Some(row) {
                    rows.remove(&key);
                }
            }
        ));
    }

    // The row the list shows a work in, if it's bound to it
    fn work_row(&self, key: &str) -> Option<gtk::Box> {
        self.imp().work_rows.borrow().get(key)?.upgrade()
    }

    // The position of a work in the list
    fn work_position(&self, key: &str) -> Option<u32> {
        let store = self.imp().works.borrow().clone()?;
        (0..store.n_items()).find(|&i| {
            store
                .item(i)
                .and_downcast::<glib::BoxedAnyObject>()
                .is_some_and(|work| work.borrow::<library::WorkInfo>().key == key)
        })
    }

    // Whether a work shows the covers of its albums
    pub fn is_work_open(&self, key: &str) -> bool {
        self.imp()
            .expanded_work
            .borrow()
            .as_ref()
            .is_some_and(|work| work.key == key)
    }

    // Opens a work to show the covers of its albums, closing the one that
    // was open, or closes it if it's open
    pub fn toggle_work(&self, work: &library::WorkInfo) {
        if self.is_work_open(&work.key) {
            self.close_work(true);
        } else {
            self.open_work(work.key.clone(), work.name.clone(), true, None);
        }
    }

    fn close_work(&self, animate: bool) {
        let imp = self.imp();
        imp.covers_generation
            .set(imp.covers_generation.get().wrapping_add(1));
        let Some(work) = imp.expanded_work.take() else {
            return;
        };
        if let Some(row) = self.work_row(&work.key) {
            self.update_work_row(&row, &work.key, animate);
        }
    }

    // Reads the covers of a work's albums, in a separate thread, and then
    // opens it, which closes the work that was open in the same frame. The
    // albums are those of its recordings that the search matches, which
    // are taken whole, as a recording is one piece of music. Opened by the
    // user, it slides open, and the list glides to show its covers. The
    // cover of `album`, if any, then takes the focus
    fn open_work(&self, key: String, name: String, by_user: bool, album: Option<String>) {
        let imp = self.imp();
        let generation = imp.covers_generation.get().wrapping_add(1);
        imp.covers_generation.set(generation);
        let filter = self.search_filter_for_works();
        let hidden = imp.hidden_folders.borrow().clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let read_key = key.clone();
                let Ok((recordings, counts)) = gio::spawn_blocking(move || {
                    let recordings = library::work_recordings(&read_key, &filter, &hidden);
                    let work = TrackFilter {
                        work: Some(read_key),
                        ..Default::default()
                    };
                    (recordings, library::search_albums(&work, &hidden))
                })
                .await
                else {
                    return;
                };
                let imp = this.imp();
                if imp.covers_generation.get() != generation {
                    return;
                }
                let covers = this.work_covers(&key, recordings, &counts);
                let work = ExpandedWork {
                    key: key.clone(),
                    name,
                    covers,
                };
                let before = imp.expanded_work.replace(Some(work));
                if let Some(before) = before.filter(|before| before.key != key)
                    && let Some(row) = this.work_row(&before.key)
                {
                    this.update_work_row(&row, &before.key, by_user);
                }
                if let Some(row) = this.work_row(&key) {
                    this.update_work_row(&row, &key, by_user);
                    if by_user {
                        this.show_when_open(&row);
                    }
                    if let Some(album) = album {
                        focus_cover(&row, &album);
                    }
                }
            }
        ));
    }

    // One cover for each album the work is recorded on, in the order of
    // its recordings, by year, with the performers and the year of the
    // first one there, or else the album's artist
    fn work_covers(
        &self,
        key: &str,
        recordings: Vec<library::WorkRecording>,
        counts: &HashMap<String, (u32, u32)>,
    ) -> Vec<WorkCover> {
        let mut covers: Vec<WorkCover> = Vec::new();
        for recording in recordings {
            let Some(first) = recording.paths.first() else {
                continue;
            };
            let favorite = FavoriteRecording {
                work_key: key.to_string(),
                album_id: recording.album_id.clone(),
                uri: gio::File::for_path(first).uri().to_string(),
            };
            if let Some(cover) = covers
                .iter_mut()
                .find(|cover| cover.album.id_str() == recording.album_id)
            {
                cover.recordings.push(favorite);
                continue;
            }
            let Some(album) = self.album(&recording.album_id) else {
                continue;
            };
            let (matching, total) = counts.get(&recording.album_id).copied().unwrap_or_default();
            let performers = if recording.performers.is_empty() {
                album.artist()
            } else {
                recording.performers
            };
            let subtitle = [Some(performers), recording.year]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            covers.push(WorkCover {
                album,
                matching,
                total,
                subtitle,
                recordings: vec![favorite],
                favorite: false,
            });
        }
        // On the main thread, as the favorites are read there
        for cover in &mut covers {
            cover.favorite = cover
                .recordings
                .iter()
                .any(play_stats::is_favorite_recording);
        }
        covers
    }

    // Opens the row of a work while it's the open one, with the covers of
    // its albums, and closes it otherwise
    fn update_work_row(&self, row: &gtk::Box, key: &str, animate: bool) {
        self.update_item_state(row, key);
        let expanded = self.imp().expanded_work.borrow();
        let open = expanded.as_ref().filter(|work| work.key == key);
        let bin = row.last_child().and_downcast::<RevealBin>();
        match open {
            Some(work) => {
                let bin = bin.unwrap_or_else(|| {
                    let bin = self.work_covers_bin();
                    row.append(&bin);
                    bin
                });
                fill_work_covers(self, &bin, work);
                bin.set_revealed(true, animate);
            }
            None => {
                if let Some(bin) = bin {
                    bin.set_revealed(false, animate);
                }
            }
        }
    }

    // The list's item that a work's row is in, which has the focus, tells
    // screen readers whether the work is open, and isn't lit up as a whole
    // under the pointer then. It's only taken while the row is on screen:
    // while the row is bound for the first time, the list doesn't have the
    // item yet, which is still floating, and a reference to it would sink
    // it, so that the list would lose it when the reference is dropped
    fn update_item_state(&self, row: &impl IsA<gtk::Widget>, key: &str) {
        if !row.is_mapped() {
            return;
        }
        let Some(item) = row.parent() else {
            return;
        };
        let open = self.is_work_open(key);
        item.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
        if open {
            item.add_css_class("expanded");
        } else {
            item.remove_css_class("expanded");
        }
    }

    // The covers of the albums of a work, which open under its line. They
    // do what the covers of the grid do, but take the work's tracks; the
    // arrows go from them to the work above and the one below, and a click
    // between them doesn't close the work, as a click on its line does
    fn work_covers_bin(&self) -> RevealBin {
        let flowbox = gtk::FlowBox::builder()
            .homogeneous(true)
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .min_children_per_line(1)
            .max_children_per_line(12)
            .css_classes(["work-covers"])
            .build();
        flowbox.connect_child_activated(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, child| {
                if let Some(cell) = child.child().and_downcast::<AlbumCell>()
                    && let Some(album) = cell.album()
                {
                    this.activate_album(&album, this.taken_by(&cell));
                }
            }
        ));
        // Enter opens the album of the cover with the focus, as the list
        // would take it to close the work. Up from the first line of covers
        // goes back to the work, and Down from the last one to the next
        // work, as the arrows go from work to work; the flow box's
        // `keynav-failed` says Down for both (GTK 4.22)
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            flowbox,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                if !modifiers.is_empty() {
                    return glib::Propagation::Proceed;
                }
                let up = match key {
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                        return match flowbox.focus_child().and_downcast::<gtk::FlowBoxChild>() {
                            Some(child) => {
                                child.activate();
                                glib::Propagation::Stop
                            }
                            None => glib::Propagation::Proceed,
                        };
                    }
                    gdk::Key::Up | gdk::Key::KP_Up => true,
                    gdk::Key::Down | gdk::Key::KP_Down => false,
                    _ => return glib::Propagation::Proceed,
                };
                if !leaves_covers(&flowbox, up) {
                    return glib::Propagation::Proceed;
                }
                let key = flowbox
                    .child_at_index(0)
                    .and_then(|child| child.child())
                    .and_downcast::<AlbumCell>()
                    .and_then(|cell| cell.work())
                    .map(|work| work.key);
                let Some(pos) = key.and_then(|key| this.work_position(&key)) else {
                    return glib::Propagation::Proceed;
                };
                let pos = if up { pos } else { pos + 1 };
                let list = &this.imp().works_list;
                if list.model().is_some_and(|model| pos < model.n_items()) {
                    list.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
                }
                glib::Propagation::Stop
            }
        ));
        flowbox.add_controller(keys);
        utils::setup_menu_key(
            &flowbox,
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cell: &AlbumCell| this.request_album_menu(cell)
            ),
        );
        let bin = RevealBin::new(&flowbox);
        let click = gtk::GestureClick::new();
        click.connect_released(|gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        bin.add_controller(click);
        bin
    }

    // Once a work has opened, the list glides to show its covers, as far as
    // its line stays in view; its row is laid out open at the frame after
    fn show_when_open(&self, row: &gtk::Box) {
        let Some(bin) = row.last_child().and_downcast::<RevealBin>() else {
            return;
        };
        let laid_out = Cell::new(false);
        bin.add_tick_callback(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            row,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move |bin, _| {
                if !bin.is_revealed() {
                    return glib::ControlFlow::Break;
                }
                if bin.progress() < 1.0 || !laid_out.replace(true) {
                    return glib::ControlFlow::Continue;
                }
                this.glide_to_row(&row);
                glib::ControlFlow::Break
            }
        ));
    }

    fn glide_to_row(&self, row: &gtk::Box) {
        let imp = self.imp();
        let list = &*imp.works_list;
        let (Some(bounds), Some(adjustment)) = (row.compute_bounds(list), list.vadjustment())
        else {
            return;
        };
        let below = bounds.y() + bounds.height() - list.height() as f32;
        let by = below.min(bounds.y());
        if by <= 0.0 {
            return;
        }
        let target = adjustment.value() + f64::from(by);
        let glide = queue_row::glide_adjustment(list, &adjustment, target);
        if let Some(before) = imp.works_glide.replace(Some(glide)) {
            before.pause();
        }
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
            // The work that was open closes, as the list starts again
            self.close_work(false);
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
        // A cover with the focus keeps it, though the list gives it to the
        // work's item when its work is replaced, e.g. as one of its
        // recordings was marked as a favorite from the cover's menu
        let focused = utils::focused_child::<AlbumCell>(&*imp.works_list)
            .filter(|cell| cell.work().is_some())
            .and_then(|cell| cell.album())
            .map(|album| album.id());
        store.splice(start as u32, removed as u32, &added);

        // The open work stays open, with its covers read again, as the
        // albums or the favorites can have changed, unless it's gone
        let open = imp
            .expanded_work
            .borrow()
            .as_ref()
            .map(|work| (work.key.clone(), work.name.clone()));
        if let Some((key, name)) = open {
            if self.work_position(&key).is_some() {
                self.open_work(key, name, false, focused);
            } else {
                self.close_work(false);
            }
        }
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
        let Some(line) = row.first_child().and_downcast::<gtk::Box>() else {
            return;
        };
        let heart = gtk::Image::builder()
            .icon_name("heart-filled-symbolic")
            .css_classes(["favorite-work"])
            .visible(false)
            .build();
        heart.update_property(&[gtk::accessible::Property::Label(&i18n("Favorite"))]);
        if let Some(name) = line.first_child() {
            line.insert_child_after(&heart, Some(&name));
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
            &line,
            clone!(
                #[weak]
                line,
                #[strong]
                open,
                move |x, y| open(line.upcast_ref(), x, y)
            ),
        );
        let menu = gio::SimpleAction::new("menu", None);
        menu.connect_activate(clone!(
            #[weak]
            line,
            move |_, _| {
                let (x, y) = (
                    f64::from(line.width()) / 2.0,
                    f64::from(line.height()) / 2.0,
                );
                open(line.upcast_ref(), x, y);
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
            .and_then(|line| line.first_child())
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

// The key of the work of an item of the list of works
fn work_key(item: &gtk::ListItem) -> Option<String> {
    let work = item.item().and_downcast::<glib::BoxedAnyObject>()?;
    Some(work.borrow::<library::WorkInfo>().key.clone())
}

// Shows the covers of a work's albums in its row, keeping the cells that
// show the same albums, so that the one with the focus keeps it when the
// work's covers are read again, e.g. after one was marked as a favorite
fn fill_work_covers(view: &LibraryView, bin: &RevealBin, work: &ExpandedWork) {
    let Some(flowbox) = bin.child().and_downcast::<gtk::FlowBox>() else {
        return;
    };
    flowbox.update_property(&[gtk::accessible::Property::Label(&i18n_k(
        // Translators: `{work}` must be left unmodified; it is expanded to
        // the name of a work, whose albums are listed
        "Recordings of {work}",
        &[("work", &work.name)],
    ))]);
    let cell_at = |i: usize| {
        flowbox
            .child_at_index(i as i32)
            .and_then(|child| child.child())
            .and_downcast::<AlbumCell>()
    };
    let same = (0..=work.covers.len()).all(|i| match (cell_at(i), work.covers.get(i)) {
        (Some(cell), Some(cover)) => {
            cell.album().as_ref() == Some(&cover.album)
                && cell
                    .work()
                    .is_some_and(|cell_work| cell_work.key == work.key)
        }
        (None, None) => true,
        _ => false,
    });
    if !same {
        flowbox.remove_all();
        for cover in &work.covers {
            let cell = AlbumCell::new(WORK_COVER_MIN, WORK_COVER_MAX);
            view.setup_cell(&cell, false);
            cell.show_album(&cover.album);
            flowbox.append(&cell);
        }
    }
    for (i, cover) in work.covers.iter().enumerate() {
        let Some(cell) = cell_at(i) else {
            continue;
        };
        cell.set_subtitle(&cover.subtitle);
        cell.set_matching(cover.matching);
        cell.set_total(cover.total);
        cell.set_favorite(cover.favorite);
        cell.set_work(Some(CellWork {
            key: work.key.clone(),
            name: work.name.clone(),
            recordings: cover.recordings.clone(),
        }));
    }
}

// Whether the cover with the focus is on the first line of the covers, for
// going up, or on the last, for going down, so that the arrow leaves them
fn leaves_covers(flowbox: &gtk::FlowBox, up: bool) -> bool {
    let top = |child: &gtk::Widget| child.compute_bounds(flowbox).map(|bounds| bounds.y());
    let Some(focus) = flowbox.focus_child().as_ref().and_then(top) else {
        return false;
    };
    let mut lines = std::iter::successors(flowbox.first_child(), WidgetExt::next_sibling)
        .filter_map(|child| top(&child));
    if up {
        lines.all(|line| line >= focus - 1.0)
    } else {
        lines.all(|line| line <= focus + 1.0)
    }
}

// Gives the focus to the cover of an album among those of a work's row
fn focus_cover(row: &gtk::Box, album: &str) {
    let Some(flowbox) = row
        .last_child()
        .and_downcast::<RevealBin>()
        .and_then(|bin| bin.child())
        .and_downcast::<gtk::FlowBox>()
    else {
        return;
    };
    let child =
        std::iter::successors(flowbox.first_child(), WidgetExt::next_sibling).find(|child| {
            child
                .first_child()
                .and_downcast::<AlbumCell>()
                .and_then(|cell| cell.album())
                .is_some_and(|cell_album| cell_album.id_str() == album)
        });
    if let Some(child) = child {
        child.grab_focus();
    }
}
