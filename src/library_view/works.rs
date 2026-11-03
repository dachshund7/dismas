// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The Works page: the list of the works the search finds, under the names
// of their composers, which takes the place of the grid. A work opens, one
// at a time, to show a cover for each of its recordings, which takes the
// recording's tracks from its album, as the Tracks page takes those a
// search matches

use super::{work_row::WorkRow, *};
use crate::{
    album_cell::CellWork, list_scroll, play_stats::FavoriteRecording, reveal_bin::RevealBin,
};

// The sizes the covers of the albums of an open work grow between, at each
// step of the zoom, as for the grid's (`COVER_SIZES`): smaller, as they're
// in a list, and none overlapping the next
pub(super) const WORK_COVER_SIZES: [(i32, i32); 4] = [(60, 80), (80, 107), (120, 160), (160, 213)];

// The work of the list that's open, and the covers of its recordings
#[derive(Debug)]
pub(crate) struct ExpandedWork {
    key: String,
    name: String,
    covers: Vec<WorkCover>,
}

// A work to go to once the works are read for the search as it is, with
// the cover to point out, and whether the search was cleared for it
// already, as it hid the work
#[derive(Debug)]
pub(crate) struct PendingWork {
    key: String,
    cover: Option<CoverRef>,
    cleared: bool,
}

// A work the user opens, whose covers are being read, with how it opens
// and the cover to point out, see `open_work()`
#[derive(Debug)]
pub(crate) struct UserOpening {
    key: String,
    opening: Opening,
    cover: Option<CoverRef>,
}

// A cover among those of an open work: the one of the recording of a track
// of an album, or without the track, or when it's in none, the first one
// of the album
#[derive(Clone, Debug)]
pub(crate) struct CoverRef {
    pub(crate) album: String,
    pub(crate) track: Option<PathBuf>,
}

// The cover of a recording of a work: a copy of its album, selected on its
// own, see `Album::recording_copy()`, and the album the grid shows, which
// tells whether the copy can be kept when the covers are read again; the
// recording's tracks, of the album's, who performs it and when, what it is
// if it's of an arrangement listed under the work, and whether it's a
// favorite
#[derive(Debug)]
struct WorkCover {
    album: Album,
    source: Album,
    paths: Arc<[PathBuf]>,
    total: u32,
    subtitle: String,
    recording: FavoriteRecording,
    arrangement: Option<Arrangement>,
    favorite: bool,
}

impl WorkCover {
    // How many tracks it takes from its album
    fn matching(&self) -> u32 {
        u32::try_from(self.paths.len()).unwrap_or(u32::MAX)
    }
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

    // What `found` gives for the first work of the list, at its position,
    // that it gives something for
    fn find_work<T>(&self, found: impl Fn(u32, &library::WorkInfo) -> Option<T>) -> Option<T> {
        let store = self.imp().works.borrow().clone()?;
        (0..store.n_items()).find_map(|i| {
            let item = store.item(i).and_downcast::<glib::BoxedAnyObject>()?;
            found(i, &item.borrow::<library::WorkInfo>())
        })
    }

    // The work that a work is listed under, itself or the work it arranges
    fn listing(&self, key: &str) -> Option<library::WorkInfo> {
        self.find_work(|_, work| {
            let listed = work.key == key || work.arrangements.iter().any(|a| a == key);
            listed.then(|| work.clone())
        })
    }

    // The position of a work in the list
    fn work_position(&self, key: &str) -> Option<u32> {
        self.find_work(|i, work| (work.key == key).then_some(i))
    }

    // A work of the list, as the list has it
    fn work_info(&self, key: &str) -> Option<library::WorkInfo> {
        self.find_work(|_, work| (work.key == key).then(|| work.clone()))
    }

    // The albums of the covers of the open work, in their order: copies, one
    // for each recording, which are selected on their own
    pub(super) fn open_work_albums(&self) -> Vec<Album> {
        let work = self.imp().expanded_work.borrow();
        work.iter()
            .flat_map(|work| &work.covers)
            .map(|cover| cover.album.clone())
            .collect()
    }

    // How many tracks the covers of the open work selected take
    pub(super) fn open_work_tracks(&self, selected: &HashSet<SelectionKey>) -> Option<u32> {
        let work = self.imp().expanded_work.borrow();
        let covers = &work.as_ref()?.covers;
        Some(
            covers
                .iter()
                .filter(|cover| selected.contains(&cover.album.selection_key()))
                .map(WorkCover::matching)
                .sum(),
        )
    }

    // The albums of the covers of the open work selected, in their order,
    // each with what its recording takes from it
    pub(super) fn selected_recordings(&self) -> Vec<(String, TrackFilter)> {
        let work = self.imp().expanded_work.borrow();
        let Some(work) = work.as_ref() else {
            return Vec::new();
        };
        work.covers
            .iter()
            .filter(|cover| cover.album.selected())
            .map(|cover| {
                let (filter, _) = taken_from_recording(cover.paths.clone(), work.name.clone());
                (cover.album.id(), filter)
            })
            .collect()
    }

    // Whether a work shows the covers of its albums
    pub(crate) fn is_work_open(&self, key: &str) -> bool {
        self.imp()
            .expanded_work
            .borrow()
            .as_ref()
            .is_some_and(|work| work.key == key)
    }

    // Opens a work to show the covers of its albums, closing the one that
    // was open, or closes it if it's open
    pub(crate) fn toggle_work(&self, work: &library::WorkInfo) {
        if self.is_work_open(&work.key) {
            self.close_work(true);
        } else {
            self.open_work(work, Opening::Toggled, None);
        }
    }

    // Closes the open work, leaving selection mode, as its covers were the
    // ones selected; before it's closed, so that they're unselected
    fn close_work(&self, animate: bool) {
        let imp = self.imp();
        imp.covers_generation
            .set(imp.covers_generation.get().wrapping_add(1));
        imp.opening.take();
        if imp.expanded_work.borrow().is_none() {
            return;
        }
        self.set_selection_mode(false);
        let Some(work) = imp.expanded_work.take() else {
            return;
        };
        self.update_select_button();
        if let Some(row) = self.work_row(&work.key) {
            self.update_work_row(&row, &work.key, animate);
        }
    }

    // Reads the covers of a work's recordings, in a separate thread, and
    // then opens it, which closes the work that was open in the same frame.
    // The recordings are those the search matches, its own and those of the
    // arrangements listed under it, which are taken whole, as a recording is
    // one piece of music. Opened by the user, it slides open, and the list
    // glides to show its covers; the `cover`, if any, is then pointed out.
    // Read again, the `cover` only takes the focus back. Covers selected
    // among those of another work are unselected, and the same work read
    // again keeps those of the recordings still there.
    //
    // Covers read before the latest opening are dropped. When the works are
    // read again meanwhile, a work the user opens is opened again, see
    // `show_works()`, rather than dropped for the work that was open
    fn open_work(&self, work: &library::WorkInfo, opening: Opening, cover: Option<CoverRef>) {
        let imp = self.imp();
        let by_user = opening != Opening::ReadAgain;
        let key = work.key.clone();
        let keys = work.keys();
        let name = work.name.clone();
        let generation = imp.covers_generation.get().wrapping_add(1);
        imp.covers_generation.set(generation);
        imp.opening.replace(by_user.then(|| UserOpening {
            key: key.clone(),
            opening,
            cover: cover.clone(),
        }));
        let filter = self.search_filter_for_works();
        let hidden = imp.hidden_folders.borrow().clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let read = gio::spawn_blocking(move || {
                    let recordings = library::work_recordings(&keys, &filter, &hidden);
                    // The number of each album's tracks, for the badges
                    let work = TrackFilter {
                        works: keys,
                        ..Default::default()
                    };
                    (recordings, library::search_albums(&work, &hidden))
                })
                .await;
                let imp = this.imp();
                if imp.covers_generation.get() != generation {
                    return;
                }
                // The opening is over, even if its covers couldn't be read:
                // left behind, it would be opened again the next time the
                // works are read, without the user asking
                imp.opening.take();
                let Ok((recordings, counts)) = read else {
                    return;
                };
                let read_again = this.is_work_open(&key);
                // Before the other work's covers go, so that they're
                // unselected
                if !read_again {
                    this.set_selection_mode(false);
                }
                let covers = this.work_covers(&key, recordings, &counts);
                if read_again {
                    // Those of the recordings gone are no longer selected
                    let shown: HashSet<SelectionKey> = covers
                        .iter()
                        .map(|cover| cover.album.selection_key())
                        .collect();
                    imp.selected.borrow_mut().retain(|key| shown.contains(key));
                    this.update_selection();
                }
                let work = ExpandedWork {
                    key: key.clone(),
                    name,
                    covers,
                };
                let before = imp.expanded_work.replace(Some(work));
                let before = before.filter(|before| before.key != key);
                this.update_select_button();
                if let Some(before) = before
                    && let Some(row) = this.work_row(&before.key)
                {
                    this.update_work_row(&row, &before.key, by_user);
                }
                // A row the list hasn't made comes in open when it does
                if let Some(row) = this.work_row(&key) {
                    this.update_work_row(&row, &key, by_user);
                }
                if by_user {
                    this.show_when_open(&key, cover, opening == Opening::GoneTo);
                } else if let Some(cover) =
                    cover.and_then(|cover| this.work_row(&key)?.cover_of(&cover))
                {
                    cover.grab_focus();
                }
            }
        ));
    }

    // One cover for each recording of the work `key`, in their order, by
    // year, with its performers and its year, or else the album's artist,
    // so an album with two recordings, another performance or an
    // arrangement, has a cover for each. The covers of the arrangements
    // listed under the work come after its own, and say first what they
    // are. When the work is read again, a recording of the same album keeps
    // its copy of the album, so that its cell is kept, see
    // `fill_work_covers()`; a new copy is selected if its recording was
    fn work_covers(
        &self,
        key: &str,
        recordings: Vec<library::WorkRecording>,
        counts: &HashMap<String, (u32, u32)>,
    ) -> Vec<WorkCover> {
        let imp = self.imp();
        // The copies of the albums shown, by the URIs of their recordings
        let mut shown: HashMap<String, (Album, Album)> = imp
            .expanded_work
            .borrow()
            .iter()
            .filter(|work| work.key == key)
            .flat_map(|work| &work.covers)
            .map(|cover| {
                let copy = (cover.source.clone(), cover.album.clone());
                (cover.recording.uri.clone(), copy)
            })
            .collect();
        let mut covers: Vec<WorkCover> = Vec::new();
        for recording in recordings {
            let Some(first) = recording.paths.first() else {
                continue;
            };
            let Some(source) = self.album(&recording.album_id) else {
                continue;
            };
            let uri = gio::File::for_path(first).uri().to_string();
            let album = match shown.remove(&uri) {
                Some((was, copy)) if was == source => copy,
                _ => {
                    let copy = source.recording_copy(&uri);
                    copy.set_selected(imp.selected.borrow().contains(&copy.selection_key()));
                    copy
                }
            };
            let total = counts
                .get(&recording.album_id)
                .map_or(0, |&(_, total)| total);
            let performers = if recording.performers.is_empty() {
                source.artist()
            } else {
                recording.performers
            };
            let subtitle = [
                recording.arrangement.as_ref().map(Arrangement::shown),
                Some(performers),
                recording.year,
            ]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            covers.push(WorkCover {
                album,
                source,
                paths: recording.paths.into(),
                total,
                subtitle,
                // By the work of its own tracks, so that the recording of an
                // arrangement is among the arrangement's favorites
                recording: FavoriteRecording {
                    work_key: recording.work_key,
                    album_id: recording.album_id,
                    uri,
                },
                arrangement: recording.arrangement,
                favorite: false,
            });
        }
        covers.sort_by_key(|cover| cover.arrangement.is_some());
        // On the main thread, as the favorites are read there
        for cover in &mut covers {
            cover.favorite = play_stats::is_favorite_recording(&cover.recording);
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
    // tall for that has its line at the top.
    //
    // When its row is in view and its covers slide open, where the line ends
    // is known from the height of the covers before they open, and at each
    // frame of their slide, the list puts the line that much of the way
    // there, with the slide's easing, which keeps it in place as a work above
    // it closes at the same time. A work reopened as it closed slides on from
    // the share that still showed, which counts as none of the way, or the
    // line would jump that much at the first frame. When the list is
    // scrolled away from the row as it slides, or while its covers are
    // read, the user goes somewhere else, and the list is left where it is.
    //
    // Otherwise, as when Go to Work goes to a work out of view, or to one
    // open already, the list glides there as Show the Playing Track glides to
    // a track, rather than jumping: at each frame, it goes its part of what's
    // left of the way, to where the line goes if its row is laid out, or else
    // to where the row is reckoned to be. The list only lays out the rows
    // near its view, so the work's row is made as the list comes near it, and
    // comes in open. The glide only ends with the line in its place: when
    // its last step aimed at where the row was reckoned to be, the line is
    // placed at the next frame, from where the row was laid out, and when
    // the row isn't there either, the list is scrolled to it, and the line
    // is placed from there.
    //
    // The row is laid out open at the frame after the last, when the
    // `cover`, if any, is pointed out. A work `gone_to` without one takes
    // the focus then; one opened from its row has it already, or the user
    // took it somewhere else meanwhile.
    //
    // The line is put in its place after the list is laid out, not before.
    // A scroll keeps one item of the list in place, and lays out the others
    // from it: GTK picks the item at a point of the view that depends on how
    // far down the list is, and keeps either its top or its bottom where it
    // was. Scrolled before the layout, the line stayed in its place when the
    // top of its row or a row above it was kept, but went up by what the
    // covers grew in that frame when the bottom of its row or a row below it
    // was, and as the item kept changed from frame to frame, the covers
    // shook. Scrolled after the layout, when no row changes height any more,
    // the list is laid out again before it's drawn, as the frame clock
    // repeats its layout while one is asked for, with the line in its place.
    //
    // The frame clock only lays out a frame when something asks for it. The
    // covers ask at each frame of their slide, but nothing does as the list
    // glides, so the list asks for a layout at each frame until the line is
    // placed.
    //
    // Only one work opens at a time, so a placement still running when
    // another one starts is stopped first, and so is the glide bringing the
    // cover pointed out by the last one into view. Both would move the list
    // in the same frames, and their moves would add up. So is the list going
    // on by itself after a fling, see `list_scroll::stop_kinetic_scrolling()`
    fn show_when_open(&self, key: &str, cover: Option<CoverRef>, gone_to: bool) {
        let imp = self.imp();
        if let Some(placing) = imp.placing.take() {
            placing.remove();
        }
        if let Some(glide) = imp.cover_glide.take() {
            adw::prelude::AnimationExt::pause(&glide);
        }
        // A work clicked whose row the user scrolled away from while its
        // covers were read
        let shown = self.shown_work_row(key);
        if shown.is_none() && !gone_to {
            return;
        }
        list_scroll::stop_kinetic_scrolling(&*imp.works_list);
        let Some(pos) = self.work_position(key) else {
            return;
        };
        let list = imp.works_list.get();
        let slide = shown.and_then(|(row, start)| {
            let from = row.covers()?.progress();
            (from < 1.0).then(|| Pace::Slide {
                start,
                end: line_end(&list, &row),
                from,
            })
        });
        let key = key.to_string();
        let pace = Cell::new(None);
        // The line is placed until this callback is dropped: when it's done,
        // when the next placement removes it, or when the list is disposed
        let after_layout = Cell::new(None);
        let waited = Cell::new(0);
        let laid_out = Cell::new(false);
        let settling = Cell::new(0);
        let fell_back = Cell::new(false);
        let tick = list.add_tick_callback(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move |list, clock| {
                if !this.is_work_open(&key) {
                    return glib::ControlFlow::Break;
                }
                // A list just shown gets its size at a later frame
                if list.height() == 0 {
                    return list_scroll::wait_for_size(&waited);
                }
                // Set at the first frame, which comes once the list is
                // realized, as it may have had no frame clock before
                let pace = pace.get().unwrap_or_else(|| {
                    let new = slide.unwrap_or_else(|| {
                        let animate = list.settings().is_gtk_enable_animations();
                        Pace::Glide(list_scroll::Glide::new(clock.frame_time(), animate))
                    });
                    pace.set(Some(new));
                    let place = place_line(&this, &key, pos, new);
                    after_layout.set(Some(AfterLayout::connect(clock, place)));
                    new
                });
                let shown = this.shown_work_row(&key);
                let row = match pace {
                    Pace::Slide { .. } => {
                        // The list was scrolled away from the row, and the
                        // user goes somewhere else
                        let Some((row, _)) = shown else {
                            return glib::ControlFlow::Break;
                        };
                        let progress = row.covers().map_or(1.0, |bin| bin.progress());
                        if pace.share(progress, clock.frame_time()) < 1.0 || !laid_out.replace(true)
                        {
                            list.queue_allocate();
                            return glib::ControlFlow::Continue;
                        }
                        row
                    }
                    // The glide's last step is made in the layout of the
                    // first frame its time is up in
                    Pace::Glide { .. }
                        if pace.share(1.0, clock.frame_time()) < 1.0 || !laid_out.replace(true) =>
                    {
                        list.queue_allocate();
                        return glib::ControlFlow::Continue;
                    }
                    Pace::Glide { .. } => match shown {
                        Some((row, y))
                            if line_is_placed(list, &row, y) || settling.get() >= SETTLE_FRAMES =>
                        {
                            row
                        }
                        // The row was laid out after the last step, which
                        // aimed at where it was reckoned to be: the next
                        // layout goes the rest of the way to its line
                        Some(_) => {
                            settling.set(settling.get() + 1);
                            list.queue_allocate();
                            return glib::ControlFlow::Continue;
                        }
                        // The glide ended where the row was reckoned to be,
                        // and it wasn't there; the list knows where it is,
                        // and the next layout places the line from there
                        None => {
                            if fell_back.replace(true) {
                                return glib::ControlFlow::Break;
                            }
                            list.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                            list.queue_allocate();
                            return glib::ControlFlow::Continue;
                        }
                    },
                };
                match cover.as_ref().and_then(|cover| row.cover_of(cover)) {
                    Some(cover) => this.point_out_cover(&cover),
                    None if gone_to => {
                        if let Some(item) = utils::list_item_of(&row) {
                            item.grab_focus();
                        }
                    }
                    None => {}
                }
                glib::ControlFlow::Break
            }
        ));
        imp.placing.replace(Some(tick));
    }

    // The row of a work and where it is in the list, if the list shows it.
    // The list keeps some rows out of its view, e.g. the one of the item
    // with the focus, hidden where they last were, which isn't where they
    // are now
    fn shown_work_row(&self, key: &str) -> Option<(WorkRow, f32)> {
        let row = self.work_row(key).filter(WidgetExt::is_mapped)?;
        let bounds = row
            .compute_bounds(&*self.imp().works_list)
            .filter(|bounds| bounds.height() > 0.0)?;
        Some((row, bounds.y()))
    }

    // The cover of the recording gone to with a work takes the focus, glides
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
                let glide = list_scroll::glide_adjustment(
                    list,
                    &adjustment,
                    adjustment.value() + f64::from(by),
                );
                self.imp().cover_glide.replace(Some(glide));
            }
        }
        let Some(cell) = cover.first_child() else {
            return;
        };
        cell.add_css_class("flash");
        glib::timeout_add_local_once(
            list_scroll::FLASH_DURATION,
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
    pub(crate) fn refresh_favorites(&self) {
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
        let focused = utils::focused_child::<AlbumCell>(&*imp.works_list).and_then(|cell| {
            Some(CoverRef {
                track: cell.work()?.paths.first().cloned(),
                album: cell.album()?.id(),
            })
        });
        for item in merge_works(&store, works) {
            let work = item.borrow::<library::WorkInfo>();
            if let Some(row) = self.work_row(&work.key) {
                row.show(&work);
            }
        }

        // The open work stays open, with its covers read again, as the
        // albums or the favorites can have changed, unless it's gone. A work
        // the user is opening, whose covers are still being read, is opened
        // again instead, as that reading is dropped now, and it's the work
        // that will be open. If that work is gone, the open one stays; with
        // none open, the reading of its covers is dropped all the same, or
        // they would open a work the list no longer has
        let open = imp
            .expanded_work
            .borrow()
            .as_ref()
            .map(|work| work.key.clone());
        let opening = imp.opening.take();
        let opening_work = opening
            .as_ref()
            .and_then(|opening| self.work_info(&opening.key));
        if let (Some(opening), Some(work)) = (&opening, opening_work) {
            self.open_work(&work, opening.opening, opening.cover.clone());
        } else if let Some(key) = open {
            match self.work_info(&key) {
                Some(work) => self.open_work(&work, Opening::ReadAgain, focused),
                None => self.close_work(false),
            }
        } else if opening.is_some() {
            self.close_work(false);
        }
        self.update_stack();
        self.reveal_pending_work();
    }

    // Goes to a work, as from Go to Work in the menu of a track: it opens,
    // as if it had been clicked, and the list glides to it, or to the work
    // it's listed under if it's an arrangement; then the cover of the
    // recording of `track` on `album`, if any, is pointed out, or else the
    // work takes the focus. That's once the works shown are read for the
    // search as it is, and the library, which can take a moment when the
    // page was just shown
    pub(crate) fn reveal_work(&self, key: &str, album: Option<&str>, track: Option<&Path>) {
        self.imp().pending_work.replace(Some(PendingWork {
            key: key.to_string(),
            cover: album.map(|album| CoverRef {
                album: album.to_string(),
                track: track.map(Path::to_path_buf),
            }),
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
        if let Some(work) = self.listing(&pending.key) {
            // The list glides to it once it's read, see `show_when_open()`
            imp.works_revealed
                .set(imp.works_revealed.get().wrapping_add(1));
            self.open_work(&work, Opening::GoneTo, pending.cover);
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

// What's taken from the album of a recording of a work: its tracks, by
// their `paths`, under a bar that names the work
pub(super) fn taken_from_recording(
    paths: Arc<[PathBuf]>,
    name: String,
) -> (TrackFilter, PickNames) {
    (
        TrackFilter {
            tracks: Some(paths),
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

// Shows the covers of a work's recordings in its row, keeping the cells
// that show the same copies of their albums, so that the one with the focus
// keeps it when the work's covers are read again, e.g. after one was
// marked as a favorite
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
            let cell = AlbumCell::new(|step| super::COVER_ZOOM.at(&WORK_COVER_SIZES, step));
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
        cell.set_matching(cover.matching());
        cell.set_total(cover.total);
        cell.set_favorite(cover.favorite);
        cell.set_work(Some(CellWork {
            key: work.key.clone(),
            name: work.name.clone(),
            recording: cover.recording.clone(),
            paths: cover.paths.clone(),
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

// Why a work opens: from its row, clicked or with the keys; gone to, as
// with Go to Work, which can be from out of view, see `show_when_open()`;
// or read again, as the library or the search changed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opening {
    Toggled,
    GoneTo,
    ReadAgain,
}

// How far along its way the line of a work opening is, see
// `show_when_open()`
#[derive(Clone, Copy, Debug)]
enum Pace {
    // The covers slide open in view, and the line goes from `start` to
    // `end`, in the list's coordinates, as far as they slid since they
    // showed the share `from`, with the slide's easing
    Slide { start: f32, end: f32, from: f64 },
    // The list glides to the line, as it glides to a track with Show the
    // Playing Track
    Glide(list_scroll::Glide),
}

impl Pace {
    // How far the move is, from 0 to 1, with the bin's `progress` at the
    // frame time `now`: the share of the way that the covers slid, which is
    // eased already, or the share of the glide's time, which the glide eases
    fn share(self, progress: f64, now: i64) -> f64 {
        match self {
            Self::Slide { from, .. } if from < 1.0 => {
                ((progress - from) / (1.0 - from)).clamp(0.0, 1.0)
            }
            Self::Slide { .. } => 1.0,
            Self::Glide(glide) => glide.progress(now),
        }
    }
}

// Where the line of an open work's row goes in the list, so that its covers
// are in the middle of the list, as far as the line stays in view
fn line_end(list: &gtk::ListView, row: &WorkRow) -> f32 {
    let covers = row
        .covers()
        .and_then(|bin| bin.child())
        .map_or(0, |covers| {
            covers.measure(gtk::Orientation::Vertical, row.width()).1
        });
    (list.height() as f32 / 2.0 - row.line().height() as f32 - covers as f32 / 2.0).max(0.0)
}

// Where the list of works scrolls to, so that the line of an open work's
// row, at `y` in the list, is in its place, as far as the list scrolls
fn line_target(list: &gtk::ListView, adjustment: &gtk::Adjustment, row: &WorkRow, y: f32) -> f64 {
    let off = (y - line_end(list, row)).round();
    list_scroll::within_scroll(adjustment, adjustment.value() + f64::from(off))
}

// The frames that a glide to a work is waited for to put its line in its
// place, after its time is up, as rows laid out at its last step can move
// the line: then it ends where the line is
const SETTLE_FRAMES: u32 = 5;

// Whether the line of an open work's row, at `y` in the list, is in its
// place, give or take the pixel that the list rounds its scroll to
fn line_is_placed(list: &gtk::ListView, row: &WorkRow, y: f32) -> bool {
    list.vadjustment().is_none_or(|adjustment| {
        (line_target(list, &adjustment, row, y) - adjustment.value()).abs() <= 1.0
    })
}

// The layout handler that moves the list of works so that the line of the
// work `key`, at `pos`, is as far along its way as `pace` says, see
// `show_when_open()`. It moves the list once a frame: a glide goes its part
// of the way once, and the list rounds its scroll to whole pixels, so the
// line of a slide can still be a pixel off after it's placed, and isn't
// laid out again and again for that pixel
fn place_line(
    view: &LibraryView,
    key: &str,
    pos: u32,
    pace: Pace,
) -> impl Fn(&gdk::FrameClock) + 'static {
    let key = key.to_string();
    let placed_at = Cell::new(None);
    // A glide keeps how much of its way is left
    let pace = Cell::new(pace);
    clone!(
        #[weak]
        view,
        move |clock: &gdk::FrameClock| {
            let frame = clock.frame_counter();
            if placed_at.get() == Some(frame) || !view.is_work_open(&key) {
                return;
            }
            let list = view.imp().works_list.get();
            let Some(adjustment) = list.vadjustment() else {
                return;
            };
            let shown = view.shown_work_row(&key);
            let value = match (pace.get(), shown) {
                (slide @ Pace::Slide { start, end, .. }, Some((row, y))) => {
                    let progress = row.covers().map_or(1.0, |bin| bin.progress());
                    let share = slide.share(progress, clock.frame_time());
                    let off = y - (start + (end - start) * share as f32).round();
                    if off.abs() < 1.0 {
                        return;
                    }
                    adjustment.value() + f64::from(off)
                }
                (Pace::Slide { .. }, None) => return,
                // The glide went all its way, and the row wasn't there: the
                // tick scrolls to it, which aiming at where it's reckoned
                // to be again would undo
                (Pace::Glide(glide), None) if glide.is_done() => return,
                (Pace::Glide(mut glide), shown) => {
                    let target = match shown {
                        Some((row, y)) => line_target(&list, &adjustment, &row, y),
                        None => list_scroll::estimated_value(&list, &adjustment, pos),
                    };
                    let value = glide.step(adjustment.value(), target, clock.frame_time());
                    pace.set(Pace::Glide(glide));
                    value
                }
            };
            placed_at.set(Some(frame));
            adjustment.set_value(value);
        }
    )
}

// A handler of a frame clock's layout signal, disconnected when this is
// dropped. It runs after the handlers connected without `after`, whatever
// the order they were connected in, among them the one the window's surface
// lays the window out with
struct AfterLayout(gdk::FrameClock, Option<glib::SignalHandlerId>);

impl AfterLayout {
    fn connect(clock: &gdk::FrameClock, f: impl Fn(&gdk::FrameClock) + 'static) -> Self {
        let handler = clock.connect_closure(
            "layout",
            true,
            glib::closure_local!(move |clock: &gdk::FrameClock| f(clock)),
        );
        Self(clock.clone(), Some(handler))
    }
}

impl Drop for AfterLayout {
    fn drop(&mut self) {
        if let Some(handler) = self.1.take() {
            self.0.disconnect(handler);
        }
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

    #[test]
    fn the_line_goes_all_its_way_however_the_work_opens() {
        // A work opening from closed goes as far as its covers slid open
        let slide = Pace::Slide {
            start: 0.0,
            end: 0.0,
            from: 0.0,
        };
        assert_eq!(slide.share(0.0, 0), 0.0);
        assert_eq!(slide.share(0.3, 0), 0.3);
        assert_eq!(slide.share(1.0, 0), 1.0);

        // One reopened as it closed starts at none of the way, not at the
        // share that still showed
        let reopened = Pace::Slide {
            start: 0.0,
            end: 0.0,
            from: 0.6,
        };
        assert_eq!(reopened.share(0.6, 0), 0.0);
        assert!((reopened.share(0.8, 0) - 0.5).abs() < 1e-9);
        assert_eq!(reopened.share(1.0, 0), 1.0);

        // A glide goes by its own time, from its start, whatever the covers
        // show
        let duration = list_scroll::GLIDE_DURATION.as_micros() as i64;
        let glide = Pace::Glide(list_scroll::Glide::new(1_000, true));
        assert_eq!(glide.share(0.0, 1_000), 0.0);
        assert_eq!(glide.share(1.0, 1_000 + duration / 2), 0.5);
        assert_eq!(glide.share(0.0, 1_000 + duration), 1.0);
        assert_eq!(glide.share(1.0, 1_000 + duration * 3), 1.0);

        // Without animations, it's there at once
        let jump = Pace::Glide(list_scroll::Glide::new(1_000, false));
        assert_eq!(jump.share(0.0, 1_000), 1.0);
    }
}
