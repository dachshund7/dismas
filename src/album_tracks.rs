// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The tracks of an album from the library, shown in the sidebar like the
// queue, so they can be played or added to it

use std::cell::{Cell, RefCell};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gio, glib};
use std::sync::LazyLock;

use crate::{
    audio::{Track, load_tracks},
    i18n::{i18n, ni18n_f},
    library::{self, Album},
    queue_row::QueueRow,
    utils,
    work_sections::{WorkSectionModel, work_header_factory},
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
        pub tracks_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub add_button: TemplateChild<gtk::Button>,
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
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("back").build(),
                    Signal::builder("add").build(),
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
        let sections = WorkSectionModel::new(&tracks);
        imp.tracks_view
            .set_model(Some(&gtk::NoSelection::new(Some(sections))));
        imp.tracks_view.set_factory(Some(&factory));
        imp.tracks_view
            .set_header_factory(Some(&work_header_factory(
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |start, end, ctrl| this.toggle_work(start, end, ctrl)
                ),
                None::<fn(&Track, u32)>,
            )));

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
            move |_| this.emit_by_name::<()>("play", &[&0u32])
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

    // The files of the tracks, in order; in selection mode, only the
    // selected ones
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

    // Shows the tracks of an album that contain every search word, or all
    // of them without any; the track of `reveal`, if any, is scrolled to
    // and flashes once it's read
    pub fn show_album(&self, album: &Album, search_words: &[String], reveal: Option<&gio::File>) {
        let imp = self.imp();
        self.set_selection_mode(false);

        imp.album_title.set_label(&album.title());
        imp.album_title.set_tooltip_text(Some(&album.title()));
        imp.album_artist.set_label(&album.artist());

        let Some(tracks) = imp.tracks.borrow().clone() else {
            return;
        };
        tracks.remove_all();
        imp.files.replace(Vec::new());

        // The files are in the database of the library, and reading the
        // tags of the tracks takes a moment, so both are done in separate
        // threads; the tracks are shown as they come, and showing another
        // album stops it
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        let album_id = album.id();
        let search_words = search_words.to_vec();
        let reveal = reveal.cloned();
        let mut revealed = false;
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let paths =
                    gio::spawn_blocking(move || library::album_tracks(&album_id, &search_words))
                        .await
                        .unwrap_or_default();
                if this.imp().generation.get() != generation {
                    return;
                }
                let files: Vec<gio::File> = paths.into_iter().map(gio::File::for_path).collect();
                this.imp().files.replace(files.clone());

                let receiver = load_tracks(files);
                while let Ok(data) = receiver.recv().await {
                    if this.imp().generation.get() != generation {
                        return;
                    }

                    // Along with the ones that are already read
                    let mut batch: Vec<Track> = data.into_iter().map(Track::new).collect();
                    while let Ok(data) = receiver.try_recv() {
                        batch.extend(data.map(Track::new));
                    }
                    if batch.is_empty() {
                        continue;
                    }
                    let start = tracks.n_items();
                    tracks.extend_from_slice(&batch);

                    let found = reveal.as_ref().filter(|_| !revealed).and_then(|file| {
                        let i = batch.iter().position(|track| track.file().equal(file))?;
                        Some((start + i as u32, &batch[i]))
                    });
                    if let Some((pos, track)) = found {
                        revealed = true;
                        crate::queue_row::flash_track(&this.imp().tracks_view, pos, track);
                    } else if !revealed {
                        // The list keeps its first track at the top as
                        // tracks are added, which hides the header of its
                        // work above it; once a track is revealed, it stays
                        // in view
                        if let Some(adjustment) = this.imp().tracks_view.vadjustment() {
                            adjustment.set_value(0.0);
                        }
                    }
                }
            }
        ));
    }
}
