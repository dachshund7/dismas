// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The tracks of an album from the library, shown in the sidebar like the
// queue, so they can be played or added to it; or the recordings of a work,
// each under a header with its performers

use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib};
use std::sync::LazyLock;

use crate::{
    audio::{Track, load_tracks},
    i18n::{i18n, i18n_k, ni18n_f, ni18n_k},
    library::{self, Album, TrackFilter},
    library_view::PickNames,
    queue_row::QueueRow,
    utils,
    work_sections::{SameSection, WorkSectionModel, work_header_factory},
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/dachshund7/Dismas/album-tracks.ui")]
    #[properties(wrapper_type = super::AlbumTracks)]
    pub struct AlbumTracks {
        #[template_child]
        pub back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub album_title: TemplateChild<gtk::Label>,
        #[template_child]
        pub album_artist: TemplateChild<gtk::Label>,
        #[template_child]
        pub album_info: TemplateChild<gtk::Label>,
        #[template_child]
        pub filter_bar: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub filter_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub tracks_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub buttons: TemplateChild<gtk::Box>,
        #[template_child]
        pub play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub add_button: TemplateChild<adw::SplitButton>,
        #[template_child]
        pub select_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub selected_label: TemplateChild<gtk::Label>,

        // Whether clicking a track selects it, so that only the selected
        // tracks are played or added
        #[property(get, set = Self::set_selection_mode)]
        pub selection_mode: Cell<bool>,
        // The last track selected or unselected with a click, where
        // Shift+click starts selecting from
        pub selection_anchor: glib::WeakRef<Track>,
        // Tracks are being added to the queue
        pub loading: Cell<bool>,

        pub tracks: RefCell<Option<gio::ListStore>>,
        pub files: RefCell<Vec<gio::File>>,
        // The work whose recordings are showing, in place of an album, the
        // folders whose tracks are hidden, and its recordings, with the
        // recording each track is in, by its URI
        pub work: RefCell<Option<library::WorkInfo>>,
        pub hidden: RefCell<Vec<PathBuf>>,
        pub recordings: Rc<RefCell<Vec<library::WorkRecording>>>,
        pub recording_of: Rc<RefCell<HashMap<String, usize>>>,
        // The sections of the list, and the headers of the works of an
        // album and of the recordings of a work
        pub sections: OnceCell<WorkSectionModel>,
        pub work_headers: OnceCell<gtk::ListItemFactory>,
        pub recording_headers: OnceCell<gtk::ListItemFactory>,
        // The album that is showing, and the search, and the composer and
        // performer, it was shown for, with their names
        pub album: RefCell<Option<Album>>,
        pub filter: RefCell<TrackFilter>,
        pub picks: RefCell<PickNames>,
        // While the search leaves out some of the tracks, the number of
        // the tracks that match it and the number of all of them
        pub left_out: Cell<Option<(u32, u32)>>,
        // Then, the URIs of the tracks it matches, which are the ones taken;
        // the others are dimmed. The menu next to the Add button offers to
        // take them all
        pub matching: Rc<RefCell<Option<HashSet<String>>>>,
        #[property(get)]
        pub partial: Cell<bool>,
        // Bumped for each album, to stop loading the tracks of the
        // previous one
        pub generation: Cell<u32>,
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
            self.obj().update_buttons();
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
                    // The Play button: the tracks it takes play, see
                    // `files()`
                    Signal::builder("play-shown").build(),
                    // The position of the track to start playing from
                    Signal::builder("play")
                        .param_types([u32::static_type()])
                        .build(),
                    // The menu of a track, at the given point of its row
                    Signal::builder("menu-requested")
                        .param_types([
                            QueueRow::static_type(),
                            f64::static_type(),
                            f64::static_type(),
                        ])
                        .build(),
                    // The menu of a work, at a point of its header, with
                    // its first movement
                    Signal::builder("work-menu-requested")
                        .param_types([
                            gtk::Widget::static_type(),
                            f64::static_type(),
                            f64::static_type(),
                            Track::static_type(),
                        ])
                        .build(),
                    // The menu of a recording of a work, at a point of its
                    // header, with the positions of its first track and
                    // past its last one
                    Signal::builder("recording-menu-requested")
                        .param_types([
                            gtk::Widget::static_type(),
                            f64::static_type(),
                            f64::static_type(),
                            u32::static_type(),
                            u32::static_type(),
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
    pub struct AlbumTracks(ObjectSubclass<imp::AlbumTracks>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AlbumTracks {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AlbumTracks {
    pub fn connect_back<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "back",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_add<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "add",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_play_shown<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "play-shown",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_play<F: Fn(&Self, u32) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "play",
            false,
            glib::closure_local!(move |view: &Self, start: u32| f(view, start)),
        )
    }

    pub fn connect_menu_requested<F: Fn(&Self, &QueueRow, f64, f64) + 'static>(
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

    pub fn connect_work_menu_requested<F: Fn(&Self, &gtk::Widget, f64, f64, &Track) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "work-menu-requested",
            false,
            glib::closure_local!(move |view: &Self,
                                       widget: &gtk::Widget,
                                       x: f64,
                                       y: f64,
                                       track: &Track| f(
                view, widget, x, y, track
            )),
        )
    }

    pub fn connect_recording_menu_requested<
        F: Fn(&Self, &gtk::Widget, f64, f64, u32, u32) + 'static,
    >(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "recording-menu-requested",
            false,
            glib::closure_local!(move |view: &Self,
                                       widget: &gtk::Widget,
                                       x: f64,
                                       y: f64,
                                       start: u32,
                                       end: u32| f(
                view, widget, x, y, start, end
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
        // The tracks the search doesn't match are dimmed
        factory.connect_bind(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let (Some(row), Some(track)) = (item.child(), item.item().and_downcast::<Track>())
                else {
                    return;
                };
                let matching = this.imp().matching.borrow();
                if matching
                    .as_ref()
                    .is_some_and(|matching| !matching.contains(&track.uri()))
                {
                    row.add_css_class("unmatched");
                } else {
                    row.remove_css_class("unmatched");
                }
            }
        ));

        let tracks = gio::ListStore::new::<Track>();
        let sections = WorkSectionModel::new(&tracks);
        imp.tracks_view
            .set_model(Some(&gtk::NoSelection::new(Some(sections.clone()))));
        imp.tracks_view.set_factory(Some(&factory));
        let work_headers = work_header_factory(
            clone!(
                #[weak(rename_to = this)]
                self,
                move |start, end, ctrl| this.toggle_work(start, end, ctrl)
            ),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |widget: &gtk::Widget, x, y, start| {
                    let track = this
                        .imp()
                        .tracks
                        .borrow()
                        .as_ref()
                        .and_then(|tracks| tracks.item(start))
                        .and_downcast::<Track>();
                    if let Some(track) = track {
                        this.emit_by_name::<()>("work-menu-requested", &[widget, &x, &y, &track]);
                    }
                }
            ),
            None::<(fn() -> bool, fn(&Track, u32))>,
        );
        imp.tracks_view.set_header_factory(Some(&work_headers));
        let _ = imp.sections.set(sections);
        let _ = imp.work_headers.set(work_headers);
        let _ = imp.recording_headers.set(self.recording_header_factory());

        imp.tracks.replace(Some(tracks));
    }

    fn connect_signals(&self) {
        let imp = self.imp();

        imp.back_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("back", &[])
        ));

        imp.add_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("add", &[])
        ));

        imp.play_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("play-shown", &[])
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
                    this.emit_by_name::<()>("play", &[&pos]);
                }
            }
        ));
    }

    // The tracks of the album that is showing
    pub fn tracks(&self) -> Vec<Track> {
        self.imp()
            .tracks
            .borrow()
            .as_ref()
            .map(|t| t.iter::<Track>().flatten().collect())
            .unwrap_or_default()
    }

    // Shows the new tags of a track that was updated: its row and the
    // section of its work are made again
    pub fn track_changed(&self, track: &Track) {
        let tracks = self.imp().tracks.borrow().clone();
        if let Some(tracks) = tracks
            && let Some(pos) = tracks.find(track)
        {
            tracks.items_changed(pos, 1, 1);
        }
    }

    // The files of the tracks the Play and Add buttons take, in order: the
    // ones the search matches; in selection mode, only the selected ones
    pub fn files(&self) -> Vec<gio::File> {
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
    pub fn toggle_track(&self, track: &Track) {
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

    pub fn select_all(&self) {
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

        let can_add = self.adding_enabled() && (!self.selection_mode() || n_selected > 0);
        imp.play_button.set_sensitive(can_add);
        imp.add_button.set_sensitive(can_add);
    }

    // The tooltip of the button that goes back to the selected playlist
    pub fn set_back_tooltip(&self, tooltip: &str) {
        self.imp().back_button.set_tooltip_text(Some(tooltip));
    }

    // The menu next to the button that adds the tracks, with the other
    // ways to add them
    pub fn set_add_menu(&self, menu: &gio::MenuModel) {
        self.imp().add_button.set_menu_model(Some(menu));
    }

    // The label of the button that adds the tracks to the selected playlist
    pub fn set_add_label(&self, label: &str) {
        utils::set_playlist_button_label(&self.imp().add_button, label);
    }

    pub fn adding_enabled(&self) -> bool {
        !self.imp().loading.get()
    }

    // Albums can't be added while tracks are being added to the queue
    pub fn set_adding_enabled(&self, enabled: bool) {
        self.imp().loading.set(!enabled);
        self.update_selection();
    }

    // Shows the tracks of an album that the filter matches, or all of them
    // with an empty one; `picks` names the composer and the performer the
    // filter picks, if any. The track of `reveal`, if any, is scrolled to
    // and flashes once it's read
    pub fn show_album(
        &self,
        album: &Album,
        filter: &TrackFilter,
        picks: &PickNames,
        reveal: Option<&gio::File>,
    ) {
        let imp = self.imp();
        imp.album_title.set_label(&album.title());
        imp.album_title.set_tooltip_text(Some(&album.title()));
        imp.album_artist.set_label(&album.artist());
        let info = [album.year(), album.label(), album.catalog_number()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        imp.album_info.set_label(&info);
        imp.album_info.set_visible(!info.is_empty());

        imp.album.replace(Some(album.clone()));
        imp.work.replace(None);
        imp.filter.replace(filter.clone());
        imp.picks.replace(picks.clone());
        self.load(reveal);
    }

    // Shows the recordings of a work that the filter matches, each whole,
    // leaving out the tracks in the `hidden` folders. The track of
    // `reveal`, if any, is scrolled to and flashes once it's read
    pub fn show_work(
        &self,
        work: &library::WorkInfo,
        filter: &TrackFilter,
        hidden: Vec<PathBuf>,
        reveal: Option<&gio::File>,
    ) {
        let imp = self.imp();
        imp.album_title.set_label(&work.name);
        imp.album_title
            .set_tooltip_text(Some(work.title.as_deref().unwrap_or(&work.name)));
        let composers = work
            .composers
            .iter()
            .map(|composer| composer.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        imp.album_artist.set_label(&composers);
        imp.album_info.set_visible(false);

        imp.album.replace(None);
        imp.work.replace(Some(work.clone()));
        imp.hidden.replace(hidden);
        imp.filter.replace(filter.clone());
        imp.picks.replace(PickNames::default());
        self.load(reveal);
    }

    // Whether the recordings of a work are showing, rather than an album
    pub fn shows_work(&self) -> bool {
        self.imp().work.borrow().is_some()
    }

    // The files to play from the track at `pos`, and the position to start
    // at among them: those of the album that the search matches, or all of
    // them from a track it doesn't match, which is dimmed, or the selected
    // tracks, or the recording the track is in
    pub fn files_to_play(&self, pos: u32) -> (Vec<gio::File>, u32) {
        let imp = self.imp();
        if self.selection_mode() {
            return (self.files(), pos);
        }
        if !self.shows_work() {
            let tracks = self.tracks();
            let matching = imp.matching.borrow();
            let Some(matching) = matching.as_ref() else {
                return (self.files(), pos);
            };
            let clicked = tracks.get(pos as usize);
            if clicked.is_some_and(|track| !matching.contains(&track.uri())) {
                return (tracks.iter().map(Track::file).collect(), pos);
            }
            let matched: Vec<&Track> = tracks
                .iter()
                .filter(|track| matching.contains(&track.uri()))
                .collect();
            let start = matched
                .iter()
                .position(|track| Some(*track) == clicked)
                .unwrap_or_default();
            return (
                matched.iter().map(|track| track.file()).collect(),
                start as u32,
            );
        }
        let Some(sections) = imp.sections.get() else {
            return (Vec::new(), 0);
        };
        let (start, end) = sections.section(pos);
        (self.section_files(start, end), pos.saturating_sub(start))
    }

    // The files of the tracks from `start` to before `end`
    pub fn section_files(&self, start: u32, end: u32) -> Vec<gio::File> {
        let tracks = self.imp().tracks.borrow().clone();
        let Some(tracks) = tracks else {
            return Vec::new();
        };
        (start..end.min(tracks.n_items()))
            .filter_map(|i| tracks.item(i).and_downcast::<Track>())
            .map(|track| track.file())
            .collect()
    }

    // The album of the recording the track at `pos` is in
    pub fn recording_album(&self, pos: u32) -> Option<String> {
        let imp = self.imp();
        let track = imp
            .tracks
            .borrow()
            .as_ref()?
            .item(pos)
            .and_downcast::<Track>()?;
        let index = *imp.recording_of.borrow().get(&track.uri())?;
        Some(imp.recordings.borrow().get(index)?.album_id.clone())
    }

    // The Play and Add buttons play and add the whole album, or the
    // selected tracks; each recording of a work has its own Play button
    fn update_buttons(&self) {
        let imp = self.imp();
        imp.buttons
            .set_visible(!self.shows_work() || self.selection_mode());
    }

    // The files of all the tracks, though the search leaves some out
    pub fn all_files(&self) -> Vec<gio::File> {
        self.tracks().iter().map(Track::file).collect()
    }

    // Says how many of the tracks the search matches while it leaves out
    // some, which are dimmed. With only a composer or a performer picked,
    // and no words to search for, it says how many are theirs
    fn update_filter_bar(&self) {
        let imp = self.imp();
        let Some((matching, total)) = imp.left_out.get() else {
            imp.filter_bar.set_reveal_child(false);
            return;
        };
        let picks = imp.picks.borrow();
        let words = !imp.filter.borrow().words.is_empty();
        let label = match (picks.composer.as_deref(), picks.performer.as_deref()) {
            (Some(composer), Some(performer)) if !words => ni18n_k(
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
            (None, Some(performer)) if !words => ni18n_k(
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
            (Some(composer), None) if !words => ni18n_k(
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
            _ => ni18n_f(
                // Translators: The first '{}' is the number of tracks of the
                // album that match the search, and the second the number of
                // all of them; both must be left unmodified
                "The search matches {} of {} track",
                "The search matches {} of {} tracks",
                total,
                &[&matching.to_string(), &total.to_string()],
            ),
        };
        imp.filter_label.set_label(&label);
        imp.filter_bar.set_reveal_child(true);
    }

    // Reads the tracks of the album, keeping the ones that match the
    // search, or the recordings of the work
    fn load(&self, reveal: Option<&gio::File>) {
        let imp = self.imp();
        self.set_selection_mode(false);
        self.update_buttons();

        let Some(tracks) = imp.tracks.borrow().clone() else {
            return;
        };
        let work = imp.work.borrow().clone();
        let album = imp.album.borrow().clone();
        if work.is_none() && album.is_none() {
            return;
        }
        tracks.remove_all();
        imp.files.replace(Vec::new());
        imp.matching.replace(None);
        imp.left_out.set(None);
        self.set_partial(false);
        imp.recordings.replace(Vec::new());
        imp.recording_of.replace(HashMap::new());

        // Each recording of a work is a section, under a header of its own
        let (same, headers): (Option<SameSection>, _) = if work.is_some() {
            let recording_of = imp.recording_of.clone();
            let same: SameSection = Rc::new(move |previous: &Track, track: &Track| {
                let recording_of = recording_of.borrow();
                recording_of.get(&previous.uri()) == recording_of.get(&track.uri())
            });
            (Some(same), imp.recording_headers.get())
        } else {
            (None, imp.work_headers.get())
        };
        if let Some(sections) = imp.sections.get() {
            sections.set_same_section(same);
        }
        imp.tracks_view.set_header_factory(headers);

        // The files are in the database of the library, and reading the
        // tags of the tracks takes a moment, so both are done in separate
        // threads; the tracks are shown as they come, and showing another
        // album stops it
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        let filter = imp.filter.borrow().clone();
        let hidden = imp.hidden.borrow().clone();
        let reveal = reveal.cloned();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let files = if let Some(work) = work {
                    let recordings = gio::spawn_blocking(move || {
                        library::work_recordings(&work.key, &filter, &hidden)
                    })
                    .await
                    .unwrap_or_default();
                    if this.imp().generation.get() != generation {
                        return;
                    }
                    this.update_filter_bar();
                    let files = this.show_recordings(recordings);
                    this.imp().files.replace(files.clone());
                    files
                } else {
                    let album_id = album.map(|album| album.id()).unwrap_or_default();
                    // All the tracks are shown, and with a search, those it
                    // matches are taken, and the others dimmed
                    let (all, matching) = gio::spawn_blocking(move || {
                        let all = library::album_tracks(&album_id, &TrackFilter::default());
                        let matching =
                            (!filter.is_empty()).then(|| library::album_tracks(&album_id, &filter));
                        (all, matching)
                    })
                    .await
                    .unwrap_or_default();
                    if this.imp().generation.get() != generation {
                        return;
                    }
                    let all: Vec<gio::File> = all.into_iter().map(gio::File::for_path).collect();
                    let matching = matching.filter(|matching| matching.len() < all.len());
                    let imp = this.imp();
                    if let Some(matching) = matching {
                        imp.left_out
                            .set(Some((matching.len() as u32, all.len() as u32)));
                        let files: Vec<gio::File> =
                            matching.into_iter().map(gio::File::for_path).collect();
                        imp.matching.replace(Some(
                            files.iter().map(|file| file.uri().to_string()).collect(),
                        ));
                        imp.files.replace(files);
                        this.set_partial(true);
                    } else {
                        imp.files.replace(all.clone());
                    }
                    this.update_filter_bar();
                    all
                };
                this.read_tracks(generation, files, reveal).await;
            }
        ));
    }

    fn set_partial(&self, partial: bool) {
        if self.imp().partial.replace(partial) != partial {
            self.notify_partial();
        }
    }

    // Keeps the recordings of the work, and the recording of each of their
    // tracks, and gives their files, in order
    fn show_recordings(&self, recordings: Vec<library::WorkRecording>) -> Vec<gio::File> {
        let imp = self.imp();
        let mut files = Vec::new();
        {
            let mut recording_of = imp.recording_of.borrow_mut();
            for (i, recording) in recordings.iter().enumerate() {
                for path in &recording.paths {
                    let file = gio::File::for_path(path);
                    recording_of.insert(file.uri().to_string(), i);
                    files.push(file);
                }
            }
        }
        let work = imp.work.borrow();
        if let Some(work) = work.as_ref() {
            let composers = work
                .composers
                .iter()
                .map(|composer| composer.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let count = recordings.len() as u32;
            let recordings_label = ni18n_f(
                // Translators: The '{}' must be left unmodified, and it is
                // expanded to the number of recordings of a work in the
                // music folders
                "{} recording",
                "{} recordings",
                count,
                &[&count.to_string()],
            );
            let subtitle = [composers, recordings_label]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            imp.album_artist.set_label(&subtitle);
        }
        imp.recordings.replace(recordings);
        files
    }

    // Reads the tags of the tracks, which are shown as they come, unless
    // another album or work is shown meanwhile
    async fn read_tracks(&self, generation: u32, files: Vec<gio::File>, reveal: Option<gio::File>) {
        let imp = self.imp();
        let Some(tracks) = imp.tracks.borrow().clone() else {
            return;
        };
        // A track to reveal stays in view, rather than the top
        let revealed = Rc::new(Cell::new(false));
        let receiver = load_tracks(files, false);
        while let Ok(data) = receiver.recv().await {
            if imp.generation.get() != generation {
                return;
            }

            // Along with the ones that are already read
            let mut batch: Vec<Track> = data.into_iter().map(Track::new).collect();
            while let Ok(data) = receiver.try_recv() {
                batch.extend(data.into_iter().map(Track::new));
            }
            if batch.is_empty() {
                continue;
            }
            let start = tracks.n_items();
            tracks.extend_from_slice(&batch);

            let found = reveal
                .as_ref()
                .filter(|_| !revealed.get())
                .and_then(|file| {
                    let i = batch.iter().position(|track| track.file().equal(file))?;
                    Some((start + i as u32, &batch[i]))
                });
            if let Some((pos, track)) = found {
                revealed.set(true);
                crate::queue_row::flash_track(&imp.tracks_view, pos, track);
            } else if !revealed.get() {
                // The list keeps its first track at the top as tracks are
                // added, which hides the header of its work above it, unless
                // another album is shown by then
                let still_wanted = clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    revealed,
                    #[upgrade_or]
                    false,
                    move || this.imp().generation.get() == generation && !revealed.get()
                );
                utils::scroll_to_top_when_laid_out(&imp.tracks_view, still_wanted);
            }
        }
    }

    // The headers of the recordings of a work: a line between them, their
    // conductors and performers, the year and the title of their album, a
    // button that plays them, and one with their menu, which a right click
    // or a long press on the header opens too. In selection mode, a click
    // selects their tracks, like the header of a work
    fn recording_header_factory(&self) -> gtk::ListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
                let separator = gtk::Separator::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .css_classes(["work-separator"])
                    .build();
                let performers = gtk::Label::builder()
                    .xalign(0.0)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .css_classes(["work-title"])
                    .build();
                let album = gtk::Label::builder()
                    .xalign(0.0)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .css_classes(["work-composer", "caption", "dim-label"])
                    .build();
                let text = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .hexpand(true)
                    .valign(gtk::Align::Center)
                    .build();
                text.append(&performers);
                text.append(&album);

                let play = gtk::Button::builder()
                    .icon_name("media-playback-start-symbolic")
                    .tooltip_text(i18n("Play"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat", "circular"])
                    .build();
                play.connect_clicked(clone!(
                    #[weak]
                    this,
                    #[weak]
                    header,
                    move |_| {
                        if this.adding_enabled() {
                            this.emit_by_name::<()>("play", &[&header.start()]);
                        }
                    }
                ));
                let more = gtk::Button::builder()
                    .icon_name("view-more-symbolic")
                    .tooltip_text(i18n("More"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat", "circular"])
                    .build();
                more.connect_clicked(clone!(
                    #[weak]
                    this,
                    #[weak]
                    header,
                    move |button| {
                        let (x, y) = (f64::from(button.width()) / 2.0, f64::from(button.height()));
                        this.emit_by_name::<()>(
                            "recording-menu-requested",
                            &[
                                &button.upcast_ref::<gtk::Widget>(),
                                &x,
                                &y,
                                &header.start(),
                                &header.end(),
                            ],
                        );
                    }
                ));

                let row = gtk::Box::builder().spacing(6).margin_end(6).build();
                row.append(&text);
                row.append(&play);
                row.append(&more);

                // A right click or a long press opens the menu, and a click
                // selects the tracks in selection mode
                let click = gtk::GestureClick::builder().button(0).build();
                click.connect_pressed(clone!(
                    #[weak]
                    this,
                    #[weak]
                    header,
                    #[weak]
                    text,
                    move |gesture, _, x, y| {
                        if gesture.current_button() == gdk::BUTTON_SECONDARY {
                            gesture.set_state(gtk::EventSequenceState::Claimed);
                            this.emit_by_name::<()>(
                                "recording-menu-requested",
                                &[
                                    &text.upcast_ref::<gtk::Widget>(),
                                    &x,
                                    &y,
                                    &header.start(),
                                    &header.end(),
                                ],
                            );
                        } else if gesture.current_button() == gdk::BUTTON_PRIMARY {
                            let ctrl = gesture
                                .current_event_state()
                                .contains(gdk::ModifierType::CONTROL_MASK);
                            this.toggle_work(header.start(), header.end(), ctrl);
                        }
                    }
                ));
                text.add_controller(click);
                let long_press = gtk::GestureLongPress::new();
                long_press.set_touch_only(true);
                long_press.connect_pressed(clone!(
                    #[weak]
                    this,
                    #[weak]
                    header,
                    #[weak]
                    text,
                    move |gesture, x, y| {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        this.emit_by_name::<()>(
                            "recording-menu-requested",
                            &[
                                &text.upcast_ref::<gtk::Widget>(),
                                &x,
                                &y,
                                &header.start(),
                                &header.end(),
                            ],
                        );
                    }
                ));
                text.add_controller(long_press);

                let content = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .build();
                content.append(&separator);
                content.append(&row);
                header.set_child(Some(&content));
            }
        ));
        factory.connect_bind(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
                let Some(content) = header.child() else {
                    return;
                };
                let (Some(separator), Some(row)) = (content.first_child(), content.last_child())
                else {
                    return;
                };
                let Some(text) = row.first_child() else {
                    return;
                };
                let (Some(performers), Some(album)) = (
                    text.first_child().and_downcast::<gtk::Label>(),
                    text.last_child().and_downcast::<gtk::Label>(),
                ) else {
                    return;
                };
                // Nothing to separate at the top of the list
                separator.set_visible(header.start() > 0);

                let imp = this.imp();
                let track = header.item().and_downcast::<Track>();
                let recordings = imp.recordings.borrow();
                let recording = track
                    .and_then(|track| imp.recording_of.borrow().get(&track.uri()).copied())
                    .and_then(|i| recordings.get(i));
                let Some(recording) = recording else {
                    performers.set_label("");
                    album.set_label("");
                    return;
                };
                let title = recording
                    .album
                    .clone()
                    .unwrap_or_else(|| i18n("Unknown album"));
                performers.set_label(&recording.performers);
                performers.set_visible(!recording.performers.is_empty());
                album.set_label(&match &recording.year {
                    Some(year) => i18n_k(
                        // Translators: `{year}` and `{album}` must be left
                        // unmodified; they are expanded to the year of an
                        // album with a recording of a work, and its title
                        "{year} · {album}",
                        &[("year", year), ("album", &title)],
                    ),
                    None => title,
                });
            }
        ));
        factory.upcast()
    }
}
