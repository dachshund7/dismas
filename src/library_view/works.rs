// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The Works page: the list of the works the search finds, under the names
// of their composers, which takes the place of the grid. A work opens, one
// at a time, to show the covers of the albums it's recorded on, which take
// the work's tracks from them, as the Tracks page takes those a search
// matches

use super::{work_row::WorkRow, *};
use crate::{
    album_cell::CellWork, play_stats::FavoriteRecording, queue_row, reveal_bin::RevealBin,
};

// The sizes the covers of the albums of an open work grow between, at each
// step of the zoom, as for the grid's (`COVER_SIZES`): smaller, as they're
// in a list, and none overlapping the next
pub(super) const WORK_COVER_SIZES: [(i32, i32); 4] = [(60, 80), (80, 107), (120, 160), (160, 213)];

// The work of the list that's open, with its key and those of the
// arrangements listed under it, and the covers of its albums
#[derive(Debug)]
pub struct ExpandedWork {
    key: String,
    keys: Vec<String>,
    name: String,
    covers: Vec<WorkCover>,
}

// A work to go to once the works are read for the search as it is, with
// the album whose cover to point out, and whether the search was cleared
// for it already, as it hid the work
#[derive(Debug)]
pub struct PendingWork {
    key: String,
    album: Option<String>,
    cleared: bool,
}

// The cover of an album a work is recorded on: how many of the album's
// tracks are the work's, and how many it has, who performs it there and
// when, and its recordings there, which can be favorites, and what they
// are if they're all of an arrangement listed under the work
#[derive(Debug)]
struct WorkCover {
    album: Album,
    matching: u32,
    total: u32,
    subtitle: String,
    recordings: Vec<FavoriteRecording>,
    arrangement: Option<Arrangement>,
    favorite: bool,
}

impl LibraryView {
    // The list of the works the search finds, under the names of their
    // composers, which takes the place of the grid on the Works page;
    // activating one opens or closes it
    pub(super) fn setup_works(&self) {
        let imp = self.imp();
        imp.works_list.set_factory(Some(&self.work_factory()));
        // The rows' menus open with the Menu key too
        utils::setup_menu_key(&*imp.works_list, WorkRow::request_menu);
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

    // The rows of the list of works, see `WorkRow`. The rows are all one
    // line high, as measuring wrapped text made scrolling slow; the tooltip
    // has the full title. Each row keeps the key of its work, to be found
    // when the work opens or closes, and is open while its work is
    fn work_factory(&self) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let row = WorkRow::new();
                item.set_child(Some(&row));
                utils::set_row_tooltip(row.line(), item, |object| {
                    let work = object.downcast_ref::<glib::BoxedAnyObject>()?;
                    Some(work_tooltip(&work.borrow::<library::WorkInfo>()))
                });
                // A row is bound before the list takes its item's widget,
                // which tells screen readers whether its work is open, so
                // the widget only gets it once the row is on screen
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
                row.connect_menu_requested(clone!(
                    #[weak]
                    this,
                    #[weak]
                    item,
                    move |row, x, y| {
                        if let Some(work) = item.item().and_downcast::<glib::BoxedAnyObject>() {
                            let line = row.line().upcast_ref::<gtk::Widget>();
                            this.emit_by_name::<()>("work-menu-requested", &[&work, line, &x, &y]);
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
                let row = item.child().and_downcast::<WorkRow>();
                let work = item.item().and_downcast::<glib::BoxedAnyObject>();
                let (Some(row), Some(work)) = (row, work) else {
                    return;
                };
                let key = {
                    let work = work.borrow::<library::WorkInfo>();
                    row.show(&work);
                    work.key.clone()
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
                    (item.child().and_downcast::<WorkRow>(), work_key(item))
                else {
                    return;
                };
                let mut rows = this.imp().work_rows.borrow_mut();
                if rows.get(&key).and_then(glib::WeakRef::upgrade) == Some(row) {
                    rows.remove(&key);
                }
            }
        ));
        factory
    }

    // The row the list shows a work in, if it's bound to it
    fn work_row(&self, key: &str) -> Option<WorkRow> {
        self.imp().work_rows.borrow().get(key)?.upgrade()
    }

    // The position of the work that a work is listed under, itself or the
    // work it arranges, and the work there
    fn listing(&self, key: &str) -> Option<(u32, library::WorkInfo)> {
        let store = self.imp().works.borrow().clone()?;
        (0..store.n_items()).find_map(|i| {
            let item = store.item(i).and_downcast::<glib::BoxedAnyObject>()?;
            let work = item.borrow::<library::WorkInfo>();
            let listed = work.key == key || work.arrangements.iter().any(|a| a.key == key);
            listed.then(|| (i, work.clone()))
        })
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

    // What's taken from the albums of the open work: its tracks
    pub(super) fn taken_from_open_work(&self) -> (TrackFilter, PickNames) {
        let work = self.imp().expanded_work.borrow();
        work.as_ref()
            .map(|work| taken_from_work(work.keys.clone(), work.name.clone()))
            .unwrap_or_default()
    }

    // The albums of the covers of the open work, in their order
    pub(super) fn open_work_albums(&self) -> Vec<Album> {
        let work = self.imp().expanded_work.borrow();
        work.iter()
            .flat_map(|work| &work.covers)
            .map(|cover| cover.album.clone())
            .collect()
    }

    // How many tracks the albums of the open work with these ids take
    pub(super) fn open_work_tracks(&self, ids: &HashSet<String>) -> Option<u32> {
        let work = self.imp().expanded_work.borrow();
        let covers = &work.as_ref()?.covers;
        Some(
            covers
                .iter()
                .filter(|cover| ids.contains(cover.album.id_str()))
                .map(|cover| cover.matching)
                .sum(),
        )
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
            self.open_work(work, true, None);
        }
    }

    // Closes the open work, leaving selection mode, as its covers were the
    // ones selected
    fn close_work(&self, animate: bool) {
        let imp = self.imp();
        imp.covers_generation
            .set(imp.covers_generation.get().wrapping_add(1));
        let Some(work) = imp.expanded_work.take() else {
            return;
        };
        self.set_selection_mode(false);
        self.update_select_button();
        if let Some(row) = self.work_row(&work.key) {
            self.update_work_row(&row, &work.key, animate);
        }
    }

    // Reads the covers of a work's albums, in a separate thread, and then
    // opens it, which closes the work that was open in the same frame. The
    // albums are those of its recordings that the search matches, and of
    // the recordings of the arrangements listed under it, which are taken
    // whole, as a recording is one piece of music. Opened by the
    // user, it slides open, and the list glides to show its covers; the
    // cover of `album`, if any, is then pointed out. Read again, the cover
    // of `album` only takes the focus back. Covers selected among those of
    // another work are unselected, and the same work read again keeps them
    fn open_work(&self, work: &library::WorkInfo, by_user: bool, album: Option<String>) {
        let imp = self.imp();
        let key = work.key.clone();
        let keys = work.keys();
        let name = work.name.clone();
        let generation = imp.covers_generation.get().wrapping_add(1);
        imp.covers_generation.set(generation);
        let filter = self.search_filter_for_works();
        let hidden = imp.hidden_folders.borrow().clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let read_keys = keys.clone();
                let Ok((recordings, counts)) = gio::spawn_blocking(move || {
                    let recordings = library::work_recordings(&read_keys, &filter, &hidden);
                    let work = TrackFilter {
                        works: read_keys,
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
                let covers = this.work_covers(recordings, &counts);
                let work = ExpandedWork {
                    key: key.clone(),
                    keys,
                    name,
                    covers,
                };
                let before = imp.expanded_work.replace(Some(work));
                let before = before.filter(|before| before.key != key);
                if before.is_some() {
                    this.set_selection_mode(false);
                }
                this.update_select_button();
                if let Some(before) = before
                    && let Some(row) = this.work_row(&before.key)
                {
                    this.update_work_row(&row, &before.key, by_user);
                }
                if let Some(row) = this.work_row(&key) {
                    this.update_work_row(&row, &key, by_user);
                    if by_user {
                        this.show_when_open(&row, album);
                    } else if let Some(cover) = album.and_then(|album| row.cover_of(&album)) {
                        cover.grab_focus();
                    }
                }
            }
        ));
    }

    // One cover for each album the work is recorded on, in the order of
    // its recordings, by year, with the performers and the year of the
    // first one there, or else the album's artist. The covers of the
    // arrangements listed under it come after its own, and say first what
    // they are; an album with a recording of the work itself is the work's
    fn work_covers(
        &self,
        recordings: Vec<library::WorkRecording>,
        counts: &HashMap<String, (u32, u32)>,
    ) -> Vec<WorkCover> {
        let mut covers: Vec<WorkCover> = Vec::new();
        for recording in recordings {
            let Some(first) = recording.paths.first() else {
                continue;
            };
            // By the work of its own tracks, so that the recording of an
            // arrangement is among the arrangement's favorites
            let favorite = FavoriteRecording {
                work_key: recording.work_key.clone(),
                album_id: recording.album_id.clone(),
                uri: gio::File::for_path(first).uri().to_string(),
            };
            if let Some(cover) = covers
                .iter_mut()
                .find(|cover| cover.album.id_str() == recording.album_id)
            {
                cover.recordings.push(favorite);
                if recording.arrangement.is_none() {
                    cover.arrangement = None;
                }
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
                arrangement: recording.arrangement,
                favorite: false,
            });
        }
        for cover in &mut covers {
            if let Some(arrangement) = &cover.arrangement {
                cover.subtitle = [arrangement.shown(), std::mem::take(&mut cover.subtitle)]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
            }
        }
        covers.sort_by_key(|cover| cover.arrangement.is_some());
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
    fn update_work_row(&self, row: &WorkRow, key: &str, animate: bool) {
        self.update_item_state(row, key);
        let expanded = self.imp().expanded_work.borrow();
        match expanded.as_ref().filter(|work| work.key == key) {
            Some(work) => {
                let bin = row.covers_or_insert(|| self.work_covers_bin());
                fill_work_covers(self, &bin, work);
                bin.set_revealed(true, animate);
            }
            None => {
                if let Some(bin) = row.covers() {
                    bin.set_revealed(false, animate);
                }
            }
        }
    }

    // The list's item that a work's row is in, which has the focus, tells
    // screen readers whether the work is open, and isn't lit up as a whole
    // under the pointer then
    fn update_item_state(&self, row: &impl IsA<gtk::Widget>, key: &str) {
        let Some(item) = utils::list_item_of(row) else {
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
    // do what the covers of the grid do, selection included, but take the
    // work's tracks; the arrows go from them to the work above and the one
    // below, and a click between them doesn't close the work, as a click
    // on its line does
    fn work_covers_bin(&self) -> RevealBin {
        let flowbox = gtk::FlowBox::builder()
            .homogeneous(true)
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .min_children_per_line(1)
            .max_children_per_line(12)
            // Selected covers next to each other are highlighted apart
            .row_spacing(6)
            .column_spacing(6)
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

    // As a work opens, the list glides along, so that its covers end in the
    // middle of the list, as far as its line stays in view, so a work too
    // tall for that has its line at the top. Where the line ends is known
    // from the height of the covers before they open, and at each frame of
    // their slide, the list puts the line that much of the way there, with
    // the slide's easing, which keeps it in place as a work above it closes
    // at the same time. Its row is laid out open at the frame after the
    // last, when the cover of `album`, if any, is pointed out
    fn show_when_open(&self, row: &WorkRow, album: Option<String>) {
        let list = self.imp().works_list.get();
        let Some(bin) = row.covers() else {
            return;
        };
        let line = row.line();
        let Some(start) = row.compute_bounds(&list).map(|bounds| bounds.y()) else {
            return;
        };
        let covers = bin.child().map_or(0, |covers| {
            covers.measure(gtk::Orientation::Vertical, row.width()).1
        });
        let end =
            (list.height() as f32 / 2.0 - line.height() as f32 - covers as f32 / 2.0).max(0.0);
        let laid_out = Cell::new(false);
        bin.add_tick_callback(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            list,
            #[weak]
            row,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move |bin, _| {
                if !bin.is_revealed() {
                    return glib::ControlFlow::Break;
                }
                let progress = bin.progress();
                if let (Some(bounds), Some(adjustment)) =
                    (row.compute_bounds(&list), list.vadjustment())
                {
                    let wanted = start + (end - start) * progress as f32;
                    adjustment.set_value(adjustment.value() + f64::from(bounds.y() - wanted));
                }
                if progress < 1.0 || !laid_out.replace(true) {
                    return glib::ControlFlow::Continue;
                }
                if let Some(cover) = album.as_deref().and_then(|album| row.cover_of(album)) {
                    this.point_out_cover(&cover);
                }
                glib::ControlFlow::Break
            }
        ));
    }

    // The cover of the album gone to with a work takes the focus, glides
    // into view if the work's covers go on below the list, and flashes, as
    // a track gone to does
    fn point_out_cover(&self, cover: &gtk::Widget) {
        cover.grab_focus();
        let list = &*self.imp().works_list;
        if let (Some(bounds), Some(adjustment)) = (cover.compute_bounds(list), list.vadjustment()) {
            let below = bounds.y() + bounds.height() - list.height() as f32;
            let by = if bounds.y() < 0.0 {
                bounds.y()
            } else {
                below.max(0.0)
            };
            if by != 0.0 {
                queue_row::glide_adjustment(list, &adjustment, adjustment.value() + f64::from(by));
            }
        }
        let Some(cell) = cover.first_child() else {
            return;
        };
        cell.add_css_class("flash");
        glib::timeout_add_local_once(
            queue_row::FLASH_DURATION,
            clone!(
                #[weak]
                cell,
                move || cell.remove_css_class("flash")
            ),
        );
    }

    // Gives the list of works a new model, which it shows from the top, with
    // a section for each composer, or composers, as the works come sorted
    // by them
    fn set_works_model(&self) -> gio::ListStore {
        let imp = self.imp();
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sections = WorkSectionModel::with_same_section(
            &store,
            same_section_of(
                |previous: &glib::BoxedAnyObject, work: &glib::BoxedAnyObject| {
                    let previous = previous.borrow::<library::WorkInfo>();
                    let work = work.borrow::<library::WorkInfo>();
                    previous.composer_keys().eq(work.composer_keys())
                },
            ),
        );
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
                    // A favorite arrangement makes the work it's listed
                    // under one
                    for work in &mut works {
                        work.favorite = work.is_among(&favorites);
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
    // favorite or unmarked
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
        // scrolled to the top, unless a work was gone to meanwhile
        let store = if new_search {
            // The work that was open closes, as the list starts again
            self.close_work(false);
            let store = self.set_works_model();
            let revealed = imp.works_revealed.get();
            utils::scroll_to_top_when_laid_out(
                &imp.works_list,
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[upgrade_or]
                    false,
                    move || this.imp().works_revealed.get() == revealed
                ),
            );
            store
        } else {
            let Some(store) = imp.works.borrow().clone() else {
                return;
            };
            store
        };
        // A cover with the focus keeps it, though the list gives it to the
        // work's item when its work changes, e.g. as one of its recordings
        // was marked as a favorite from the cover's menu
        let focused = utils::focused_child::<AlbumCell>(&*imp.works_list)
            .filter(|cell| cell.work().is_some())
            .and_then(|cell| cell.album())
            .map(|album| album.id());
        for item in merge_works(&store, works) {
            let work = item.borrow::<library::WorkInfo>();
            if let Some(row) = self.work_row(&work.key) {
                row.show(&work);
            }
        }

        // The open work stays open, with its covers read again, as the
        // albums or the favorites can have changed, unless it's gone
        let open = imp
            .expanded_work
            .borrow()
            .as_ref()
            .map(|work| work.key.clone());
        if let Some(key) = open {
            match self.listing(&key).filter(|(_, work)| work.key == key) {
                Some((_, work)) => self.open_work(&work, false, focused),
                None => self.close_work(false),
            }
        }
        self.update_stack();
        self.reveal_pending_work();
    }

    // Goes to a work, as from Go to Work in the menu of a track: the list
    // scrolls to it, or to the work it's listed under if it's an
    // arrangement, it takes the focus, and it opens, pointing out the cover
    // of `album`, if any, as if it had been clicked. That's once the works
    // shown are read for the search as it is, and the library, which can
    // take a moment when the page was just shown
    pub fn reveal_work(&self, key: &str, album: Option<&str>) {
        self.imp().pending_work.replace(Some(PendingWork {
            key: key.to_string(),
            album: album.map(ToString::to_string),
            cleared: false,
        }));
        self.reveal_pending_work();
    }

    // Goes to the work asked for, if the works are read for the search and
    // the library as they are; `show_works` calls it again once they are.
    // When the work isn't among them, the search, the composer and the
    // performer picked, and showing only the favorites are cleared once, as
    // they can hide it, and the works are read again; a work that still
    // isn't there isn't in the library any more
    fn reveal_pending_work(&self) {
        let imp = self.imp();
        let filter = self.wanted_filter();
        let read = imp.works_filter.borrow().as_ref()
            == Some(&(filter.clone(), imp.works_generation.get()));
        if !read {
            return;
        }
        let Some(mut pending) = imp.pending_work.take() else {
            return;
        };
        if let Some((pos, work)) = self.listing(&pending.key) {
            imp.works_revealed
                .set(imp.works_revealed.get().wrapping_add(1));
            imp.works_list
                .scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
            utils::focus_when_laid_out(&*imp.works_list);
            self.open_work(&work, true, pending.album);
            return;
        }
        if pending.cleared || (filter.is_empty() && !imp.favorites_button.is_active()) {
            return;
        }
        pending.cleared = true;
        imp.pending_work.replace(Some(pending));
        // Turning off the heart reads the works again itself
        let favorites = imp.favorites_button.is_active();
        self.clear_search();
        if !favorites {
            self.update_works();
        }
    }
}

// The number of the recordings of a work, e.g. "3 recordings"
fn recordings_label(n: u32) -> String {
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

// The number of the arrangements of a work listed under it, e.g. "2
// arrangements", whose covers say what they are
fn arrangements_label(n: usize) -> String {
    let n = u32::try_from(n).unwrap_or(u32::MAX);
    ni18n_f(
        // Translators: The '{}' must be left unmodified, and it is
        // expanded to the number of arrangements of a work in the music
        // folders, whose recordings are listed with those of the work
        "With {} arrangement",
        "With {} arrangements",
        n,
        &[&n.to_string()],
    )
}

// The tooltip of a work's row: its full title, what it arranges, the
// collection it's in, the number of its recordings, and the number of the
// arrangements listed under it, a line each
fn work_tooltip(work: &library::WorkInfo) -> String {
    let recordings = recordings_label(work.recordings);
    let arrangements =
        (!work.arrangements.is_empty()).then(|| arrangements_label(work.arrangements.len()));
    let arrangement = work.arrangement.as_ref().map(|arrangement| {
        i18n_k(
            // Translators: `{work}` must be left unmodified; it is
            // expanded to the title of the work an arrangement
            // arranges
            "Arrangement of {work}",
            &[("work", &arrangement.of)],
        )
    });
    [
        Some(work.title.as_deref().unwrap_or(&work.name)),
        arrangement.as_deref(),
        work.collection.as_deref(),
        Some(&recordings),
        arrangements.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
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

// What's taken from the albums of a work: its tracks, and those of the
// arrangements listed under it, by their `keys`, under a bar that names it
pub(super) fn taken_from_work(keys: Vec<String>, name: String) -> (TrackFilter, PickNames) {
    (
        TrackFilter {
            works: keys,
            ..Default::default()
        },
        PickNames {
            work: Some(name),
            ..Default::default()
        },
    )
}

// Puts the works in the list in the place of those it shows. The list keeps
// its place by an item, and goes back to its top when that item is taken
// out, as when a scan changed works above and below the ones on screen, so
// the items of the works that are still there are kept, and those of the
// works that changed are changed in place, and returned, for their rows to
// show them; the works that are new or that moved are put in, and the
// others taken out
fn merge_works(store: &gio::ListStore, works: Vec<library::WorkInfo>) -> Vec<glib::BoxedAnyObject> {
    let keys: HashSet<String> = works.iter().map(|work| work.key.clone()).collect();
    let mut placed = HashSet::new();
    let item_at = |pos: u32| store.item(pos).and_downcast::<glib::BoxedAnyObject>();
    // The place of the next work, the number of items from there that go,
    // and the works to put in their place, in one splice
    let mut pos = 0;
    let mut gone = 0;
    let mut new = Vec::new();
    let mut changed = Vec::new();
    for work in works {
        let kept = loop {
            let Some(item) = item_at(pos + gone) else {
                break None;
            };
            let key = item.borrow::<library::WorkInfo>().key.clone();
            if key == work.key {
                break Some(item);
            }
            // A work that comes later stays for now
            if keys.contains(&key) && !placed.contains(&key) {
                break None;
            }
            gone += 1;
        };
        placed.insert(work.key.clone());
        let Some(item) = kept else {
            new.push(glib::BoxedAnyObject::new(work));
            continue;
        };
        if gone > 0 || !new.is_empty() {
            store.splice(pos, gone, &new);
        }
        pos += new.len() as u32;
        gone = 0;
        new.clear();
        if *item.borrow::<library::WorkInfo>() != work {
            item.replace(work);
            store.items_changed(pos, 1, 1);
            changed.push(item);
        }
        pos += 1;
    }
    let rest = store.n_items() - pos;
    if rest > 0 || !new.is_empty() {
        store.splice(pos, rest, &new);
    }
    changed
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
            let cell = AlbumCell::new(|step| super::at_zoom_step(&WORK_COVER_SIZES, step));
            view.setup_cell(&cell, false);
            cell.show_album(&cover.album);
            cover
                .album
                .bind_property("selected", &cell, "selected")
                .sync_create()
                .build();
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
            keys: work.keys.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn work(key: &str, recordings: u32) -> library::WorkInfo {
        library::WorkInfo {
            key: key.to_string(),
            name: key.to_string(),
            title: None,
            collection: None,
            arrangement: None,
            arrangements: Vec::new(),
            composers: Vec::new(),
            recordings,
            favorite: false,
        }
    }

    fn items(store: &gio::ListStore) -> Vec<glib::BoxedAnyObject> {
        (0..store.n_items())
            .filter_map(|i| store.item(i).and_downcast())
            .collect()
    }

    fn works_of(store: &gio::ListStore) -> Vec<(String, u32)> {
        items(store)
            .iter()
            .map(|item| {
                let work = item.borrow::<library::WorkInfo>();
                (work.key.clone(), work.recordings)
            })
            .collect()
    }

    #[test]
    fn merged_works_keep_the_items_of_those_still_there() {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        merge_works(
            &store,
            ["a", "x", "b", "c", "d"].map(|key| work(key, 1)).to_vec(),
        );
        let before = items(&store);

        // "x" is gone, "d" changed, "c" moved up, "n" is new
        let works = vec![
            work("a", 1),
            work("c", 1),
            work("b", 1),
            work("n", 1),
            work("d", 3),
        ];
        let changed = merge_works(&store, works);
        assert_eq!(
            works_of(&store),
            [("a", 1), ("c", 1), ("b", 1), ("n", 1), ("d", 3)].map(|(k, n)| (k.to_string(), n))
        );
        let after = items(&store);
        // The works still there keep their items, changed or not
        assert_eq!(after[0], before[0]);
        assert_eq!(after[2], before[2]);
        assert_eq!(after[4], before[4]);
        assert_eq!(changed, [before[4].clone()]);
        // A work that moved gets another
        assert_ne!(after[1], before[3]);

        merge_works(&store, Vec::new());
        assert_eq!(store.n_items(), 0);
    }
}
