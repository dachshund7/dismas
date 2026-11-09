// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The tracks of an album from the library, or of a playlist that isn't
// selected, shown in the sidebar like the queue, so they can be played or
// added to it. What playing them does is up to the window, which knows
// what they are

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gio, glib};
use std::sync::LazyLock;

use crate::{
    audio::{Track, load_tracks},
    booklets,
    i18n::{i18n, i18n_k, ni18n_f, ni18n_k},
    library::{self, Album, Disc, TrackFilter},
    library_view::PickNames,
    list_scroll,
    queue_row::QueueRow,
    utils,
    work_sections::{WorkSectionModel, same_section_of, shares_section, work_header_factory},
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/dachshund7/Dismas/album-tracks.ui")]
    #[properties(wrapper_type = super::AlbumTracks)]
    pub(crate) struct AlbumTracks {
        #[template_child]
        pub(crate) back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) album_title: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) album_artist: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) album_info: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) favorite_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub(crate) booklet_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) booklets_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(crate) filter_bar: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub(crate) filter_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) show_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) tracks_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub(crate) buttons: TemplateChild<gtk::Box>,
        #[template_child]
        pub(crate) play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) add_button: TemplateChild<adw::SplitButton>,
        #[template_child]
        pub(crate) select_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) selected_label: TemplateChild<gtk::Label>,

        // Whether clicking a track selects it, so that only the selected
        // tracks are played or added
        #[property(get, set = Self::set_selection_mode)]
        pub(crate) selection_mode: Cell<bool>,
        // The last track selected or unselected with a click, where
        // Shift+click starts selecting from
        pub(crate) selection_anchor: glib::WeakRef<Track>,
        // Tracks are being added to the queue
        pub(crate) loading: Cell<bool>,

        // The id of the album that is showing, none for a playlist
        pub(crate) album_id: RefCell<Option<String>>,
        pub(crate) tracks: RefCell<Option<gio::ListStore>>,
        pub(crate) files: RefCell<Vec<gio::File>>,
        // Whether the album is on more than one disc, whose sections then
        // start with each disc
        pub(crate) multi_disc: Rc<Cell<bool>>,
        // The search, and the composer and performer, the album that is
        // showing was shown for, with their names
        pub(crate) filter: RefCell<TrackFilter>,
        pub(crate) picks: RefCell<PickNames>,
        // While the search leaves out some of the tracks, which aren't
        // shown, how many it matches and how many the album has
        pub(crate) matching: Cell<Option<(u32, u32)>>,
        // While the search leaves out some of the tracks, all the files of
        // the album, in its order, which Show Whole Album lists
        pub(crate) album_files: RefCell<Vec<PathBuf>>,
        // Once Show Whole Album is pressed, where each track goes, as the
        // tracks that weren't listed go in among the others
        pub(crate) places: RefCell<Option<Places>>,
        // How many loaders are still reading tracks for the list
        pub(crate) readers: Cell<u32>,
        // A track to reveal hasn't been read yet
        pub(crate) reveal_pending: Cell<bool>,
        // While the list glides to the track to reveal, which takes the
        // focus once it gets there, the glide, stopped when other tracks are
        // shown, as it would steer the new ones to where it was going
        pub(crate) reveal_scroll: RefCell<Option<gtk::TickCallbackId>>,
        // How far the filter bar has got in going away after Show Whole
        // Album, see `hide_filter_bar()`
        pub(crate) bar_closing: RefCell<BarClosing>,
        // A key pressed on Show Whole Album hasn't been let go of
        pub(crate) key_down: Cell<bool>,
        // Bumped for each album, to stop loading the tracks of the
        // previous one
        pub(crate) generation: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumTracks {
        const NAME: &'static str = "DismasAlbumTracks";
        type Type = super::AlbumTracks;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("albumtracks");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl AlbumTracks {
        fn set_selection_mode(&self, selection_mode: bool) {
            if self.selection_mode.replace(selection_mode) == selection_mode {
                return;
            }
            if !selection_mode {
                self.obj().unselect_all();
                self.selection_anchor.set(None);
            }
            if selection_mode {
                self.tracks_view.add_css_class("selecting");
            } else {
                self.tracks_view.remove_css_class("selecting");
            }
            self.obj().update_selection();
            self.obj().notify_selection_mode();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for AlbumTracks {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.setup_list();
            obj.connect_signals();
            // They name the selected playlist
            utils::wrap_tooltip(&*self.back_button);
            utils::wrap_tooltip(&*self.add_button);
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("back").build(),
                    Signal::builder("add").build(),
                    // The heart was clicked, to mark the album as a
                    // favorite or unmark it
                    Signal::builder("favorite-toggled")
                        .param_types([bool::static_type()])
                        .build(),
                    // The tracks the Play button takes play, see `files()`,
                    // from the position of the track to start playing from,
                    // and whether that track was clicked
                    Signal::builder("play")
                        .param_types([u32::static_type(), bool::static_type()])
                        .build(),
                    // The menu of a track, at the given point of its row
                    Signal::builder("menu-requested")
                        .param_types([
                            QueueRow::static_type(),
                            f64::static_type(),
                            f64::static_type(),
                        ])
                        .build(),
                    // The menu of a work, at a point of a widget of its
                    // header, with the header and its first movement
                    Signal::builder("work-menu-requested")
                        .param_types([
                            gtk::ListHeader::static_type(),
                            gtk::Widget::static_type(),
                            f64::static_type(),
                            f64::static_type(),
                            Track::static_type(),
                        ])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for AlbumTracks {}
}

glib::wrapper! {
    pub(crate) struct AlbumTracks(ObjectSubclass<imp::AlbumTracks>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AlbumTracks {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AlbumTracks {
    pub(crate) fn connect_back<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "back",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub(crate) fn connect_add<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "add",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub(crate) fn connect_favorite_toggled<F: Fn(&Self, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "favorite-toggled",
            false,
            glib::closure_local!(move |view: &Self, favorite: bool| f(view, favorite)),
        )
    }

    pub(crate) fn connect_play<F: Fn(&Self, u32, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "play",
            false,
            glib::closure_local!(move |view: &Self, start: u32, clicked: bool| f(
                view, start, clicked
            )),
        )
    }

    pub(crate) fn connect_menu_requested<F: Fn(&Self, &QueueRow, f64, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "menu-requested",
            false,
            glib::closure_local!(move |view: &Self, row: &QueueRow, x: f64, y: f64| f(
                view, row, x, y
            )),
        )
    }

    pub(crate) fn connect_work_menu_requested<
        F: Fn(&Self, &gtk::ListHeader, &gtk::Widget, f64, f64, &Track) + 'static,
    >(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "work-menu-requested",
            false,
            glib::closure_local!(move |view: &Self,
                                       header: &gtk::ListHeader,
                                       widget: &gtk::Widget,
                                       x: f64,
                                       y: f64,
                                       track: &Track| f(
                view, header, widget, x, y, track
            )),
        )
    }

    fn setup_list(&self) {
        let imp = self.imp();

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let row = QueueRow::default();
                row.connect_menu_requested(clone!(
                    #[weak]
                    this,
                    move |row, x, y| {
                        this.emit_by_name::<()>("menu-requested", &[row, &x, &y]);
                    }
                ));
                row.connect_select_requested(clone!(
                    #[weak]
                    this,
                    move |row| {
                        if let Some(track) = row.track() {
                            this.toggle_track(&track);
                        }
                    }
                ));
                row.connect_range_select_requested(clone!(
                    #[weak]
                    this,
                    move |row| {
                        if let Some(track) = row.track() {
                            this.select_range(&track);
                        }
                    }
                ));
                row.connect_notify_local(
                    Some("selected"),
                    clone!(
                        #[weak]
                        this,
                        move |_, _| this.update_selection()
                    ),
                );
                this.bind_property("selection-mode", &row, "selection-mode")
                    .sync_create()
                    .build();
                // The row highlights its parent, so it's added first
                list_item.set_child(Some(&row));
                row.bind_list_item(list_item);
            }
        ));

        let tracks = gio::ListStore::new::<Track>();
        // A work that goes on to the next disc starts a section again
        let multi_disc = imp.multi_disc.clone();
        let sections = WorkSectionModel::with_same_section(
            &tracks,
            same_section_of(move |previous: &Track, track: &Track| {
                shares_section(previous, track)
                    && (!multi_disc.get() || previous.disc() == track.disc())
            }),
        );
        imp.tracks_view
            .set_model(Some(&gtk::NoSelection::new(Some(sections))));
        imp.tracks_view.set_factory(Some(&factory));
        utils::setup_menu_key(&*imp.tracks_view, QueueRow::request_menu);
        let work_headers = work_header_factory(
            clone!(
                #[weak(rename_to = this)]
                self,
                move |start, end, ctrl| this.toggle_work(start, end, ctrl)
            ),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |header: &gtk::ListHeader, widget: &gtk::Widget, x, y, track: &Track| {
                    this.emit_by_name::<()>(
                        "work-menu-requested",
                        &[header, widget, &x, &y, track],
                    );
                }
            ),
            None::<(fn() -> bool, fn(&Track, u32))>,
            clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or_default]
                move |track: &Track, start| this.disc_line(track, start)
            ),
        );
        imp.tracks_view.set_header_factory(Some(&work_headers));

        imp.tracks.replace(Some(tracks));
    }

    fn connect_signals(&self) {
        let imp = self.imp();

        imp.back_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("back", &[])
        ));

        utils::setup_swipe_back(
            self,
            clone!(
                #[weak(rename_to = this)]
                self,
                move || this.imp().back_button.emit_clicked()
            ),
        );

        imp.add_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("add", &[])
        ));

        // The heart is filled for a favorite, and the tooltip says what a
        // click does, as on the player's. Only a click marks the album, as
        // the heart is also set to show one marked elsewhere
        utils::setup_heart_button(
            &imp.favorite_button,
            Some((
                i18n("Add Album to Favorites"),
                i18n("Remove Album from Favorites"),
            )),
        );
        imp.favorite_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |button| {
                this.emit_by_name::<()>("favorite-toggled", &[&button.is_active()]);
            }
        ));

        imp.play_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("play", &[&0u32, &false])
        ));

        imp.show_all_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.show_whole_album()
        ));
        // Whether a key is held on Show Whole Album, whose bar stays until
        // it's let go of, see `show_whole_album()`. These handlers run
        // before the button's own, which can click it as a key is let go of
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, _, _| {
                this.imp().key_down.set(true);
                glib::Propagation::Proceed
            }
        ));
        keys.connect_key_released(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _, _| this.key_let_go()
        ));
        imp.show_all_button.add_controller(keys);
        // A key let go of where the button no longer has the focus doesn't
        // reach it
        imp.show_all_button.connect_has_focus_notify(clone!(
            #[weak(rename_to = this)]
            self,
            move |button| {
                if !button.has_focus() {
                    this.key_let_go();
                }
            }
        ));

        imp.select_all_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.select_all()
        ));

        // Clicking a track plays the album from there, or selects it
        imp.tracks_view.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            move |view, pos| {
                if this.selection_mode() {
                    let track = view
                        .model()
                        .and_then(|m| m.item(pos))
                        .and_downcast::<Track>();
                    if let Some(track) = track {
                        this.toggle_track(&track);
                    }
                } else if this.adding_enabled() {
                    this.emit_by_name::<()>("play", &[&pos, &true]);
                }
            }
        ));
    }

    // The tracks of the album that is showing
    pub(crate) fn tracks(&self) -> Vec<Track> {
        self.imp()
            .tracks
            .borrow()
            .as_ref()
            .map(|t| t.iter::<Track>().flatten().collect())
            .unwrap_or_default()
    }

    // The track at `pos` among them
    pub(crate) fn track(&self, pos: u32) -> Option<Track> {
        self.imp()
            .tracks
            .borrow()
            .as_ref()
            .and_then(|t| t.item(pos))
            .and_downcast()
    }

    // Shows the new tags of a track that was updated: its row and the
    // section of its work are made again
    pub(crate) fn track_changed(&self, track: &Track) {
        let tracks = self.imp().tracks.borrow().clone();
        if let Some(tracks) = tracks
            && let Some(pos) = tracks.find(track)
        {
            tracks.items_changed(pos, 1, 1);
        }
    }

    // The files of the tracks the Play and Add buttons take, in order: the
    // ones the search matches; in selection mode, only the selected ones
    pub(crate) fn files(&self) -> Vec<gio::File> {
        if self.selection_mode() {
            self.tracks()
                .iter()
                .filter(|t| t.selected())
                .map(Track::file)
                .collect()
        } else {
            self.imp().files.borrow().clone()
        }
    }

    // Selects or unselects a track, entering selection mode if needed
    pub(crate) fn toggle_track(&self, track: &Track) {
        if self.selection_mode() {
            track.set_selected(!track.selected());
        } else {
            self.set_selection_mode(true);
            track.set_selected(true);
        }
        self.imp().selection_anchor.set(Some(track));
        self.update_selection();
    }

    // Selects or unselects the movements of a work, from its header: in
    // selection mode, or entering it with Ctrl+click
    fn toggle_work(&self, start: u32, end: u32, ctrl: bool) {
        if !ctrl && !self.selection_mode() {
            return;
        }
        let Some(model) = self.imp().tracks_view.model() else {
            return;
        };
        self.set_selection_mode(true);
        if let Some(last) = utils::toggle_track_section(model.upcast_ref(), start, end) {
            self.imp().selection_anchor.set(Some(&last));
        }
        self.update_selection();
    }

    // Selects the tracks from the last one clicked to this one; without a
    // last one, it toggles the track like a click
    fn select_range(&self, track: &Track) {
        let anchor = self.imp().selection_anchor.upgrade();
        let model = self.imp().tracks_view.model();
        let selected = match (anchor, model) {
            (Some(anchor), Some(model)) => {
                utils::select_track_range(model.upcast_ref(), &anchor, track)
            }
            _ => false,
        };

        if selected {
            self.update_selection();
        } else {
            self.toggle_track(track);
        }
    }

    pub(crate) fn select_all(&self) {
        self.set_selection_mode(true);
        for track in self.tracks() {
            track.set_selected(true);
        }
        self.update_selection();
    }

    fn unselect_all(&self) {
        for track in self.tracks() {
            track.set_selected(false);
        }
    }

    fn update_selection(&self) {
        let imp = self.imp();
        let n_selected = self.tracks().iter().filter(|t| t.selected()).count() as u32;
        let label = if n_selected == 0 {
            i18n("No track selected")
        } else {
            ni18n_f(
                // Translators: The '{}' must be left unmodified, and
                // it is expanded to the number of tracks selected
                "{} track selected",
                "{} tracks selected",
                n_selected,
                &[&n_selected.to_string()],
            )
        };
        imp.selected_label.set_label(&label);

        let can_add = self.adding_enabled()
            && !imp.files.borrow().is_empty()
            && (!self.selection_mode() || n_selected > 0);
        imp.play_button.set_sensitive(can_add);
        imp.add_button.set_sensitive(can_add);
    }

    // The tooltip of the button that goes back to the selected playlist
    pub(crate) fn set_back_tooltip(&self, tooltip: &str) {
        self.imp().back_button.set_tooltip_text(Some(tooltip));
    }

    // The menu next to the button that adds the tracks, with the other
    // ways to add them
    pub(crate) fn set_add_menu(&self, menu: &gio::MenuModel) {
        self.imp().add_button.set_menu_model(Some(menu));
    }

    // The label of the button that adds the tracks to the selected playlist
    pub(crate) fn set_add_label(&self, label: &str) {
        utils::set_playlist_button_label(&self.imp().add_button, label);
    }

    pub(crate) fn adding_enabled(&self) -> bool {
        !self.imp().loading.get()
    }

    // Albums can't be added while tracks are being added to the queue
    pub(crate) fn set_adding_enabled(&self, enabled: bool) {
        self.imp().loading.set(!enabled);
        self.update_selection();
    }

    // Shows the tracks of an album that the filter matches, or all of them
    // with an empty one, leaving out the tracks in the `hidden` folders;
    // `picks` names the composer and the performer the filter picks, if
    // any. The track of `reveal`, if any, is scrolled to and flashes once
    // it's read
    pub(crate) fn show_album(
        &self,
        album: &Album,
        filter: TrackFilter,
        picks: PickNames,
        hidden: Vec<PathBuf>,
        reveal: Option<&gio::File>,
    ) {
        let imp = self.imp();
        self.set_title(&album.title());
        imp.album_artist.set_label(&album.artist());
        let info = [album.year(), album.label(), album.catalog_number()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        imp.album_info.set_label(&info);
        imp.album_info.set_visible(!info.is_empty());
        imp.album_artist.set_visible(true);
        imp.album_id.replace(Some(album.id()));
        imp.favorite_button.set_visible(true);
        imp.favorite_button.set_active(album.favorite());

        imp.filter.replace(filter);
        imp.picks.replace(picks);
        self.load(album.id(), hidden, reveal);
    }

    // The id of the album that is showing, none for a playlist
    pub(crate) fn album_id(&self) -> Option<String> {
        self.imp().album_id.borrow().clone()
    }

    // Lists all the tracks of the album that is showing, rather than only
    // the ones a search or a pick matched, leaving out the same folders;
    // the page, the grid and the Tracks toggle stay as they are, as the
    // toggle leaves the album that shows alone too, and going back goes
    // where it went. The files are those the album was shown with, so that
    // Play and Add take the tracks listed, whatever the library has done
    // since. Only the tracks the search left out are read, while the ones
    // it matched go on being read if they still are, and each goes in its
    // place among the others, which stay as they are, with the tracks
    // selected: the list keeps the track at its top where it is as tracks
    // come in above it, and that track takes the focus from the bar's
    // button, which hides, see `hide_filter_bar()`
    fn show_whole_album(&self) {
        let imp = self.imp();
        // The button can be clicked again while the bar slides away, when
        // the whole album is already being read
        if imp.matching.get().is_none() {
            return;
        }
        let all = imp.album_files.take();
        let matched: HashSet<PathBuf> = imp
            .files
            .borrow()
            .iter()
            .filter_map(FileExt::path)
            .collect();
        let missing: Vec<gio::File> = all
            .iter()
            .filter(|path| !matched.contains(*path))
            .map(gio::File::for_path)
            .collect();
        imp.places.replace(Some(Places::new(
            &all,
            self.tracks().iter().map(|track| track.file().path()),
        )));
        imp.files
            .replace(all.iter().map(gio::File::for_path).collect());
        imp.filter.take();
        imp.picks.take();
        imp.matching.set(None);
        self.update_selection();
        // A key held on the button, which clicks it once it has been down a
        // moment, would play the track that takes the focus as it repeats
        if imp.key_down.get() {
            imp.bar_closing.replace(BarClosing::OnRelease);
        } else {
            self.hide_filter_bar();
        }

        // The tracks left out go in their places among the others
        self.spawn_reader(imp.generation.get(), missing, None, false);
    }

    // Adds tracks to the list: at its end, or once Show Whole Album is
    // pressed, each in its place on the album, those next to each other in
    // one go
    fn add_tracks(&self, tracks: &gio::ListStore, batch: Vec<Track>) {
        let mut places = self.imp().places.borrow_mut();
        let Some(album) = places.as_mut() else {
            drop(places);
            tracks.extend_from_slice(&batch);
            return;
        };
        let mut placed: Vec<(usize, Track)> = batch
            .into_iter()
            .map(|track| (album.of(&track), track))
            .collect();
        placed.sort_by_key(|(place, _)| *place);
        let (new, mut batch): (Vec<usize>, Vec<Track>) = placed.into_iter().unzip();
        let runs = insert_places(&mut album.listed, &new);
        // The places are let go of before the tracks go in, as the rows
        // bound then read the list
        drop(places);
        for (start, count) in runs {
            let run: Vec<Track> = batch.drain(..count).collect();
            tracks.splice(start as u32, 0, &run);
        }
    }

    // Hides the filter bar after Show Whole Album, then gives a track the
    // focus if it went with the bar's button. The button keeps it while the
    // bar slides away, or for as long as that would take with animations
    // off, which hide the bar at once, so that a key pressed again soon
    // after does nothing rather than play a track
    fn hide_filter_bar(&self) {
        let imp = self.imp();
        self.update_filter_bar();
        let wait = Duration::from_millis(imp.filter_bar.transition_duration().into());
        let source = glib::timeout_add_local_once(
            wait,
            clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    let imp = this.imp();
                    // The source is done, and isn't removed
                    imp.bar_closing.replace(BarClosing::Gone);
                    this.focus_wanted_track();
                }
            ),
        );
        imp.bar_closing.replace(BarClosing::Sliding(source));
    }

    // A key held on Show Whole Album was let go of, or the button lost the
    // focus, so that its bar can go if it waits for that
    fn key_let_go(&self) {
        let imp = self.imp();
        imp.key_down.set(false);
        if matches!(*imp.bar_closing.borrow(), BarClosing::OnRelease) {
            self.hide_filter_bar();
        }
    }

    // Once Show Whole Album's bar has gone, gives the focus to a track if it
    // went with the bar's button: to the one at the top of the list, or the
    // first if none is laid out yet. A track to reveal takes it instead, as
    // the list glides to it, or once it's read; and until a track is read
    // the focus waits for one, and goes to Back if the album has none
    fn focus_wanted_track(&self) {
        let imp = self.imp();
        if !self.focus_wanted() || imp.reveal_pending.get() {
            return;
        }
        if !self.focus_lost() || imp.reveal_scroll.borrow().is_some() {
            imp.bar_closing.replace(BarClosing::No);
            return;
        }
        if self.track(0).is_none() && imp.readers.get() > 0 {
            return;
        }
        imp.bar_closing.replace(BarClosing::No);
        match list_scroll::top_item(&imp.tracks_view) {
            Some(item) => {
                item.grab_focus();
            }
            None => self.focus_first_track_if_lost(),
        }
    }

    // Whether Show Whole Album's bar has gone, and a track is to take the
    // focus if it went with the bar's button
    fn focus_wanted(&self) -> bool {
        matches!(*self.imp().bar_closing.borrow(), BarClosing::Gone)
    }

    // Shows the tracks of a playlist, under its `title`, with a `caption`
    // such as what a smart playlist picks, if any; how many there are and
    // how long they last show once they're read. With `placeholders`, the
    // files that aren't there keep their places, as in the queue a saved
    // playlist is switched to, so that a track's place among them is its
    // place in the playlist
    pub(crate) fn show_files(
        &self,
        title: &str,
        caption: &str,
        files: Vec<gio::File>,
        placeholders: bool,
    ) {
        let imp = self.imp();
        self.set_title(title);
        imp.album_artist.set_label(caption);
        imp.album_artist.set_visible(!caption.is_empty());
        imp.album_info.set_visible(false);
        imp.album_id.replace(None);
        imp.favorite_button.set_visible(false);
        imp.filter.replace(TrackFilter::default());
        imp.picks.replace(PickNames::default());

        let generation = self.clear();
        imp.files.replace(files.clone());
        self.update_selection();
        if files.is_empty() {
            self.show_count();
            return;
        }
        self.spawn_reader(generation, files, None, placeholders);
    }

    // Adds files at the end of the playlist that is showing, whose tracks
    // are read again, as a track's place among them is its place in the
    // playlist
    pub(crate) fn add_files(&self, added: &[gio::File]) {
        let imp = self.imp();
        let title = imp.album_title.label();
        let mut files = imp.files.borrow().clone();
        files.extend_from_slice(added);
        self.show_files(&title, "", files, true);
    }

    // The title of the album or the playlist that is showing, which can be
    // renamed
    pub(crate) fn set_title(&self, title: &str) {
        let imp = self.imp();
        imp.album_title.set_label(title);
        imp.album_title.set_tooltip_text(Some(title));
    }

    // Says how many tracks the playlist that is showing has, and how long
    // they last, once they're read; an album shows its year and label
    // instead
    fn show_count(&self) {
        let imp = self.imp();
        if imp.album_id.borrow().is_some() {
            return;
        }
        let tracks = self.tracks();
        let seconds = tracks.iter().map(Track::duration).sum();
        let count = tracks_caption(tracks.len() as u32, seconds);
        imp.album_info.set_label(&count);
        imp.album_info.set_visible(true);
    }

    // Shows whether the album is a favorite, when it's one of the albums
    // marked or unmarked elsewhere
    pub(crate) fn show_favorite(&self, album_ids: &[String], favorite: bool) {
        let imp = self.imp();
        if imp
            .album_id
            .borrow()
            .as_ref()
            .is_some_and(|id| album_ids.contains(id))
        {
            imp.favorite_button.set_active(favorite);
        }
    }

    // Says how many of the tracks the search matches while it leaves out
    // some, which don't show. With only a composer or a performer picked,
    // or only the favorites, and no words to search for, it says how many
    // are theirs, or favorites
    fn update_filter_bar(&self) {
        let imp = self.imp();
        let Some((matching, total)) = imp.matching.get() else {
            imp.filter_bar.set_reveal_child(false);
            return;
        };
        let picks = imp.picks.borrow();
        let filter = imp.filter.borrow();
        // The tracks of a composer or a performer counted with words or the
        // favorites would only be some of theirs
        let searched = filter.query.is_some() || filter.favorites.is_some();
        let label = if let Some(work) = picks.work.as_deref() {
            ni18n_k(
                // Translators: `{n}`, `{total}` and `{work}` must be left
                // unmodified; they are expanded to the number of tracks of
                // the album that are part of a work, the number of all of
                // them, and the name of the work. The plural follows `{n}`
                "{n} of the {total} tracks is of {work}",
                "{n} of the {total} tracks are of {work}",
                matching,
                &[
                    ("n", &matching.to_string()),
                    ("total", &total.to_string()),
                    ("work", work),
                ],
            )
        } else {
            match (picks.composer.as_deref(), picks.performer.as_deref()) {
                (Some(composer), Some(performer)) if !searched => ni18n_k(
                    // Translators: `{n}`, `{total}`, `{composer}` and
                    // `{performer}` must be left unmodified; they are expanded
                    // to the number of tracks of the album by a composer with a
                    // performer, the number of all of them, the name of the
                    // composer and the name of the performer, which can be a
                    // conductor, a group or a player. The plural follows `{n}`
                    "{n} of the {total} tracks is by {composer}, with {performer}",
                    "{n} of the {total} tracks are by {composer}, with {performer}",
                    matching,
                    &[
                        ("n", &matching.to_string()),
                        ("total", &total.to_string()),
                        ("composer", composer),
                        ("performer", performer),
                    ],
                ),
                (None, Some(performer)) if !searched => ni18n_k(
                    // Translators: `{n}`, `{total}` and `{performer}` must be
                    // left unmodified; they are expanded to the number of
                    // tracks of the album with a performer, the number of all
                    // of them, and the name of the performer, which can be a
                    // conductor, a group or a player. The plural follows `{n}`
                    "{n} of the {total} tracks is with {performer}",
                    "{n} of the {total} tracks are with {performer}",
                    matching,
                    &[
                        ("n", &matching.to_string()),
                        ("total", &total.to_string()),
                        ("performer", performer),
                    ],
                ),
                (Some(composer), None) if !searched => ni18n_k(
                    // Translators: `{n}`, `{total}` and `{composer}` must be
                    // left unmodified; they are expanded to the number of
                    // tracks of the album by a composer, the number of all of
                    // them, and the name of the composer. The plural follows
                    // `{n}`
                    "{n} of the {total} tracks is by {composer}",
                    "{n} of the {total} tracks are by {composer}",
                    matching,
                    &[
                        ("n", &matching.to_string()),
                        ("total", &total.to_string()),
                        ("composer", composer),
                    ],
                ),
                (None, None) if filter.query.is_none() && filter.favorites.is_some() => ni18n_k(
                    // Translators: `{n}` and `{total}` must be left unmodified;
                    // they are expanded to the number of tracks of the album
                    // marked as favorites, and the number of all of them. The
                    // plural follows `{n}`
                    "{n} of the {total} tracks is a favorite",
                    "{n} of the {total} tracks are favorites",
                    matching,
                    &[("n", &matching.to_string()), ("total", &total.to_string())],
                ),
                _ => ni18n_f(
                    // Translators: The first '{}' is the number of tracks of the
                    // album that match the search, and the second the number of
                    // all of them; both must be left unmodified
                    "The search matches {} of {} track",
                    "The search matches {} of {} tracks",
                    total,
                    &[&matching.to_string(), &total.to_string()],
                ),
            }
        };
        imp.filter_label.set_label(&label);
        imp.filter_bar.set_reveal_child(true);
    }

    // Reads the tracks of the album, keeping the ones that match the
    // search, and leaving out those in the `hidden` folders
    fn load(&self, album_id: String, hidden: Vec<PathBuf>, reveal: Option<&gio::File>) {
        let imp = self.imp();
        // The files are in the database of the library, and reading the
        // tags of the tracks takes a moment, so both are done in separate
        // threads; the tracks are shown as they come, and showing another
        // album stops it
        let generation = self.clear();
        let filter = imp.filter.borrow().clone();
        let reveal = reveal.cloned();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                // With a search, only the tracks it matches are shown
                let (all, matching, discs) = gio::spawn_blocking(move || {
                    let (all, discs) = library::album_tracks_and_discs(&album_id, &hidden);
                    let matching = (!filter.is_empty())
                        .then(|| library::album_tracks(&album_id, &filter, &hidden));
                    (all, matching, discs)
                })
                .await
                .unwrap_or_default();
                if this.imp().generation.get() != generation {
                    return;
                }
                this.find_booklets(generation, all.clone());
                // Before the tracks come, as their headers read it
                let imp = this.imp();
                imp.multi_disc.set(discs);
                let total = all.len();
                let matching =
                    matching.filter(|matching| !matching.is_empty() && matching.len() < total);
                imp.matching.set(
                    matching
                        .as_ref()
                        .map(|matching| (matching.len() as u32, total as u32)),
                );
                let listed = match matching {
                    Some(matching) => {
                        imp.album_files.replace(all);
                        matching
                    }
                    None => all,
                };
                let files: Vec<gio::File> = listed.into_iter().map(gio::File::for_path).collect();
                imp.files.replace(files.clone());
                this.update_selection();
                this.update_filter_bar();
                this.spawn_reader(generation, files, reveal, false);
            }
        ));
    }

    // Empties the list for other tracks, stopping the reading of those
    // that showed; returns the generation of the new ones. Play and Add
    // are off until there are files, as those of an album are looked up
    // meanwhile
    fn clear(&self) -> u32 {
        let imp = self.imp();
        self.set_selection_mode(false);
        self.show_booklets(&[]);
        if let Some(tracks) = imp.tracks.borrow().as_ref() {
            tracks.remove_all();
        }
        imp.files.replace(Vec::new());
        imp.matching.set(None);
        imp.album_files.take();
        imp.places.take();
        imp.multi_disc.set(false);
        self.update_filter_bar();
        self.update_selection();
        // The glide to the track to reveal would steer the list to where it
        // was going, and the focus would go to a track of the new list
        imp.readers.set(0);
        imp.reveal_pending.set(false);
        if let Some(scroll) = imp.reveal_scroll.take() {
            scroll.remove();
        }
        if let BarClosing::Sliding(source) = imp.bar_closing.take() {
            source.remove();
        }
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        generation
    }

    // The line over the tracks of an album on several discs where a disc
    // starts, at the section that starts at `start`: its number, with its
    // subtitle, or the name of its folder
    fn disc_line(&self, track: &Track, start: u32) -> Option<String> {
        let imp = self.imp();
        if !imp.multi_disc.get() {
            return None;
        }
        let disc = track.disc();
        let previous = start
            .checked_sub(1)
            .and_then(|position| imp.tracks.borrow().as_ref()?.item(position))
            .and_downcast::<Track>();
        if previous.is_some_and(|previous| previous.disc() == disc) {
            return None;
        }
        Some(match disc {
            Disc::Number(number) => {
                let disc = i18n_k(
                    // Translators: `{n}` must be left unmodified; it is
                    // expanded to the number of a disc of an album
                    "Disc {n}",
                    &[("n", &number.to_string())],
                );
                match track.disc_subtitle() {
                    Some(subtitle) => format!("{disc} · {subtitle}"),
                    None => disc,
                }
            }
            Disc::Folder(folder) => folder
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        })
    }

    // Shows the buttons that open the booklets of the album's tracks once
    // they're found, in a separate thread, without holding up the tracks
    fn find_booklets(&self, generation: u32, paths: Vec<PathBuf>) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let found = gio::spawn_blocking(move || booklets::find(&paths))
                    .await
                    .unwrap_or_default();
                if this.imp().generation.get() == generation {
                    this.show_booklets(&found);
                }
            }
        ));
    }

    // The buttons that open the booklets of the album: one, or a menu of
    // several
    fn show_booklets(&self, found: &[PathBuf]) {
        let imp = self.imp();
        imp.booklet_button.set_visible(found.len() == 1);
        imp.booklets_button.set_visible(found.len() > 1);
        match found {
            [] => {}
            [booklet] => {
                imp.booklet_button
                    .set_action_target_value(Some(&booklets::target(booklet)));
                imp.booklet_button
                    .set_action_name(Some(booklets::OPEN_ACTION));
            }
            _ => imp
                .booklets_button
                .set_menu_model(Some(&booklets::menu(found))),
        }
    }

    // Starts reading the tags of the tracks, counting the reader at once,
    // as the focus waits for tracks while any are read
    fn spawn_reader(
        &self,
        generation: u32,
        files: Vec<gio::File>,
        reveal: Option<gio::File>,
        placeholders: bool,
    ) {
        let imp = self.imp();
        imp.readers.set(imp.readers.get() + 1);
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                this.read_tracks(generation, files, reveal, placeholders)
                    .await;
            }
        ));
    }

    // Reads the tags of the tracks, which are shown as they come, unless
    // another album is shown meanwhile; `spawn_reader()` counted it in
    // `readers`
    async fn read_tracks(
        &self,
        generation: u32,
        files: Vec<gio::File>,
        reveal: Option<gio::File>,
        placeholders: bool,
    ) {
        let imp = self.imp();
        let Some(tracks) = imp.tracks.borrow().clone() else {
            return;
        };
        // A track to reveal stays in view, rather than the top. Only this
        // reader has one: the one that reads the tracks Show Whole Album
        // adds leaves the reveal to it
        let wants_reveal = reveal.is_some();
        if wants_reveal {
            imp.reveal_pending.set(true);
        }
        let revealed = move |imp: &imp::AlbumTracks| wants_reveal && !imp.reveal_pending.get();
        let receiver = load_tracks(files, placeholders);
        while let Some(batch) = next_batch(&receiver).await {
            if imp.generation.get() != generation {
                return;
            }

            let batch: Vec<Track> = batch.into_iter().map(Track::new).collect();
            if batch.is_empty() {
                continue;
            }
            let found = reveal
                .as_ref()
                .filter(|_| imp.reveal_pending.get())
                .and_then(|file| batch.iter().find(|track| track.file().equal(file)))
                .cloned();
            let start = tracks.n_items();
            self.add_tracks(&tracks, batch);

            if let Some(track) = found {
                imp.reveal_pending.set(false);
                // Where it went, in its place once Show Whole Album is pressed
                let pos = tracks.find(&track).unwrap_or(start);
                let scroll = list_scroll::flash_track(
                    &imp.tracks_view,
                    pos,
                    &track,
                    // Not run once the glide is stopped, as other tracks show
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        move || {
                            this.imp().reveal_scroll.take();
                        }
                    ),
                );
                imp.reveal_scroll.replace(Some(scroll));
            } else if !revealed(imp) && imp.places.borrow().is_none() {
                // The list keeps its first track at the top as tracks are
                // added, which hides the header of its work above it, unless
                // another album is shown by then, or the tracks left out go
                // in, when the track at the top stays there
                let still_wanted = clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[upgrade_or]
                    false,
                    move || {
                        let imp = this.imp();
                        imp.generation.get() == generation
                            && !revealed(imp)
                            && imp.places.borrow().is_none()
                    }
                );
                utils::scroll_to_top_when_laid_out(&imp.tracks_view, still_wanted);
            }
            self.focus_wanted_track();
        }
        if imp.generation.get() != generation {
            return;
        }
        self.show_count();
        // The track to reveal would have taken the focus; without it, as
        // when its file left the library meanwhile, the focus can have gone
        // with the bar's button, which hides, and goes to the first track,
        // unless Show Whole Album gives it to one
        let unrevealed = wants_reveal && imp.reveal_pending.replace(false);
        let focus_wanted = self.focus_wanted();
        self.reader_done();
        if unrevealed && !focus_wanted {
            self.focus_first_track_if_lost();
        }
    }

    // A loader read all its tracks; the focus can have waited for them
    fn reader_done(&self) {
        let imp = self.imp();
        imp.readers.set(imp.readers.get().saturating_sub(1));
        self.focus_wanted_track();
    }

    // Gives the focus to the first track, or to the Back button without
    // any, if the focus is nowhere or in the filter bar, as it can have gone
    // with the bar's button, which hides
    fn focus_first_track_if_lost(&self) {
        let imp = self.imp();
        if !self.focus_lost() {
            return;
        }
        if self.track(0).is_some() {
            imp.tracks_view
                .scroll_to(0, gtk::ListScrollFlags::FOCUS, None);
            utils::focus_when_laid_out(&*imp.tracks_view);
        } else {
            imp.back_button.grab_focus();
        }
    }

    // Whether the window's focus is nowhere, or in the filter bar, unless
    // the bar is going after Show Whole Album, whose button keeps it until
    // it has, see `hide_filter_bar()`
    fn focus_lost(&self) -> bool {
        if !matches!(
            *self.imp().bar_closing.borrow(),
            BarClosing::No | BarClosing::Gone
        ) {
            return false;
        }
        let focus = self.root().and_then(|root| root.focus());
        focus.is_none_or(|focus| focus.is_ancestor(&*self.imp().filter_bar))
    }
}

// Where the tracks of an album go once Show Whole Album is pressed, as
// those the search left out go in among the ones listed, which stay as
// they are
#[derive(Debug, Default)]
pub(crate) struct Places {
    // The place of each file on the album
    places: HashMap<PathBuf, usize>,
    // The place of each track listed, in the list's order
    listed: Vec<usize>,
}

impl Places {
    // From the files of the album, in its order, and those of the tracks
    // listed, in the list's order, which is the album's. A track no longer
    // on the album, as it left the library while the album showed, stays
    // after the track before it; at the top of the list, it stays above
    // the second track, and the album's first track, if it wasn't listed,
    // goes in above it
    fn new(album: &[PathBuf], listed: impl IntoIterator<Item = Option<PathBuf>>) -> Self {
        let places: HashMap<PathBuf, usize> = album
            .iter()
            .enumerate()
            .map(|(place, path)| (path.clone(), place))
            .collect();
        let mut places_listed: Vec<usize> = Vec::new();
        for path in listed {
            let place = path
                .and_then(|path| places.get(&path).copied())
                .or(places_listed.last().copied())
                .unwrap_or(0);
            places_listed.push(place);
        }
        Self {
            places,
            listed: places_listed,
        }
    }

    // The place of a track on the album; one that isn't on it goes last
    fn of(&self, track: &Track) -> usize {
        track
            .file()
            .path()
            .and_then(|path| self.places.get(&path).copied())
            .unwrap_or(usize::MAX)
    }
}

// How the filter bar goes away after Show Whole Album, whose button keeps
// the focus until it has, see `AlbumTracks::hide_filter_bar()`
#[derive(Debug, Default)]
pub(crate) enum BarClosing {
    #[default]
    No,
    // A key is held on the button
    OnRelease,
    // The bar is sliding away, for as long as the source waits
    Sliding(glib::SourceId),
    // The bar has gone, and a track is to take the focus if it went with
    // the button, see `AlbumTracks::focus_wanted_track()`
    Gone,
}

// The next tracks a loader sends, along with those it has sent meanwhile,
// so that tracks read together go in together; none once it's done
async fn next_batch<T>(receiver: &async_channel::Receiver<Vec<T>>) -> Option<Vec<T>> {
    let mut batch = receiver.recv().await.ok()?;
    while let Ok(more) = receiver.try_recv() {
        batch.extend(more);
    }
    Some(batch)
}

// Where tracks go in a list of `listed` ones, given by their places on the
// album, sorted, as `new` ones are, which are added to it: the position
// and the number of each run of new tracks that go in next to each other,
// in the order to insert them, each run's position counting the runs put
// in before it. A new track goes after the listed ones with an earlier
// place, and before one with its own, which can only be a track no longer
// on the album that heads the list, see `Places::new()`
fn insert_places(listed: &mut Vec<usize>, new: &[usize]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &place in new {
        let pos = listed.partition_point(|&listed| listed < place);
        listed.insert(pos, place);
        match runs.last_mut() {
            Some((start, count)) if *start + *count == pos => *count += 1,
            _ => runs.push((pos, 1)),
        }
    }
    runs
}

// How many tracks a playlist has and how long they last, e.g. "12 tracks ·
// 1 hour 5 minutes"; the files that aren't there count as tracks, lasting
// nothing
fn tracks_caption(n_tracks: u32, seconds: u64) -> String {
    let count = if n_tracks == 0 {
        i18n("No tracks")
    } else {
        ni18n_k(
            // Translators: `{n}` must be left unmodified; it is expanded to
            // the number of tracks of a playlist
            "One track",
            "{n} tracks",
            n_tracks,
            &[("n", &n_tracks.to_string())],
        )
    };
    if seconds == 0 {
        return count;
    }
    // Rounded up, so that a short track isn't "0 minutes"
    let minutes_total = u32::try_from(seconds.div_ceil(60)).unwrap_or(u32::MAX);
    let (hours, minutes) = (minutes_total / 60, minutes_total % 60);
    let minutes_text = ni18n_f("{} minute", "{} minutes", minutes, &[&minutes.to_string()]);
    let length = if hours == 0 {
        minutes_text
    } else {
        let hours_text = ni18n_f(
            // Translators: the `{}` must be left unmodified; it is expanded
            // to the number of hours that the tracks of a playlist last
            "{} hour",
            "{} hours",
            hours,
            &[&hours.to_string()],
        );
        if minutes == 0 {
            hours_text
        } else {
            i18n_k(
                // Translators: `{hours}` and `{minutes}` must be left
                // unmodified; they are expanded to "1 hour" and "5 minutes"
                // in "1 hour 5 minutes", how long the tracks of a playlist
                // last
                "{hours} {minutes}",
                &[("hours", &hours_text), ("minutes", &minutes_text)],
            )
        }
    };
    i18n_k(
        // Translators: `{count}` and `{length}` must be left unmodified;
        // they are expanded to how many tracks a playlist has and how long
        // they last, e.g. "12 tracks" and "1 hour 5 minutes"
        "{count} · {length}",
        &[("count", &count), ("length", &length)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracks_go_in_their_places_in_runs() {
        // Tracks 2 and 5 of 0..8 were listed
        let mut listed = vec![2, 5];
        let runs = insert_places(&mut listed, &[0, 1, 3, 4, 6, 7]);
        assert_eq!(listed, (0..8).collect::<Vec<_>>());
        assert_eq!(runs, [(0, 2), (3, 2), (6, 2)]);

        // Applying the runs in order to the listed tracks gives the album
        let mut list = vec![2, 5];
        let mut new = [0, 1, 3, 4, 6, 7].into_iter();
        for (start, count) in runs {
            let run: Vec<_> = new.by_ref().take(count).collect();
            list.splice(start..start, run);
        }
        assert_eq!(list, (0..8).collect::<Vec<_>>());

        // A later batch, around a track already put in
        let mut listed = vec![0, 1, 4];
        assert_eq!(insert_places(&mut listed, &[2, 3, 5]), [(2, 2), (5, 1)]);
        assert_eq!(listed, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_track_off_the_album_at_the_top_goes_after_the_first_track() {
        let album: Vec<PathBuf> = ["a", "b", "c"].iter().map(PathBuf::from).collect();
        // The track that left the album, then the album's last one
        let mut places = Places::new(&album, [Some("gone".into()), Some("c".into())]);
        assert_eq!(places.listed, [0, 2]);
        // The album's first track goes above it, its second below it
        assert_eq!(insert_places(&mut places.listed, &[0, 1]), [(0, 1), (2, 1)]);
        assert_eq!(places.listed, [0, 0, 1, 2]);

        // Further down, it stays after the track before it
        let places = Places::new(&album, [Some("a".into()), None, Some("c".into())]);
        assert_eq!(places.listed, [0, 0, 2]);
    }

    #[test]
    fn a_track_off_the_album_keeps_its_place() {
        // The track that left the album has the place of the one before it,
        // and stays after it
        let mut listed = vec![3, 3];
        assert_eq!(insert_places(&mut listed, &[1, 4]), [(0, 1), (3, 1)]);
        assert_eq!(listed, [1, 3, 3, 4]);
    }

    #[test]
    fn the_caption_counts_the_tracks_and_their_length() {
        assert_eq!(tracks_caption(0, 0), "No tracks");
        assert_eq!(tracks_caption(1, 0), "One track");
        // Missing files, whose length isn't known
        assert_eq!(tracks_caption(3, 0), "3 tracks");
        assert_eq!(tracks_caption(1, 20), "One track · 1 minute");
        assert_eq!(tracks_caption(12, 59 * 60 + 1), "12 tracks · 1 hour");
        assert_eq!(
            tracks_caption(12, 3600 + 5 * 60),
            "12 tracks · 1 hour 5 minutes"
        );
        assert_eq!(
            tracks_caption(100, 6 * 3600 + 2 * 60),
            "100 tracks · 6 hours 2 minutes"
        );
    }
}
