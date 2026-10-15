// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The tracks of an album from the library, shown in the sidebar like the
// playlist, so they can be played or added to it

use std::cell::{Cell, RefCell};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gio, glib};
use once_cell::sync::Lazy;

use crate::{
    audio::Track,
    i18n::{i18n, ni18n_f},
    library::{self, Album},
    queue_row::QueueRow,
    work_sections::{WorkSectionModel, work_header_factory},
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/bassi/Amberol/album-tracks.ui")]
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
        // Tracks are being added to the playlist
        pub loading: Cell<bool>,

        pub tracks: RefCell<Option<gio::ListStore>>,
        pub files: RefCell<Vec<gio::File>>,
        // Bumped for each album, to stop loading the tracks of the
        // previous one
        pub generation: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumTracks {
        const NAME: &'static str = "AmberolAlbumTracks";
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
            static SIGNALS: Lazy<Vec<Signal>> = Lazy::new(|| {
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
                row.bind_list_item(list_item);
                list_item.set_child(Some(&row));
            }
        ));

        let tracks = gio::ListStore::new::<Track>();
        let sections = WorkSectionModel::new(&tracks);
        imp.tracks_view
            .set_model(Some(&gtk::NoSelection::new(Some(sections))));
        imp.tracks_view.set_factory(Some(&factory));
        imp.tracks_view
            .set_header_factory(Some(&work_header_factory()));

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

    fn tracks(&self) -> Vec<Track> {
        self.imp()
            .tracks
            .borrow()
            .as_ref()
            .map(|t| t.iter::<Track>().flatten().collect())
            .unwrap_or_default()
    }

    // The files of the tracks, in order; in selection mode, only the
    // selected ones
    pub fn files(&self) -> Vec<gio::File> {
        if self.selection_mode() {
            self.tracks()
                .iter()
                .filter(|t| t.selected())
                .map(|t| t.file())
                .collect()
        } else {
            self.imp().files.borrow().clone()
        }
    }

    // Selects or unselects a track, entering selection mode if needed
    pub fn toggle_track(&self, track: &Track) {
        if !self.selection_mode() {
            self.set_selection_mode(true);
            track.set_selected(true);
        } else {
            track.set_selected(!track.selected());
        }
        self.update_selection();
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

    pub fn adding_enabled(&self) -> bool {
        !self.imp().loading.get()
    }

    // Albums can't be added while tracks are being added to the playlist
    pub fn set_adding_enabled(&self, enabled: bool) {
        self.imp().loading.set(!enabled);
        self.update_selection();
    }

    // Shows the tracks of an album that contain every search word, or all
    // of them without any
    pub fn show_album(&self, album: &Album, search_words: &[String]) {
        let imp = self.imp();
        self.set_selection_mode(false);

        imp.album_title.set_label(&album.title());
        imp.album_title.set_tooltip_text(Some(&album.title()));
        imp.album_artist.set_label(&album.artist());

        let files: Vec<gio::File> = library::album_tracks(&album.id(), search_words)
            .into_iter()
            .map(gio::File::for_path)
            .collect();
        imp.files.replace(files.clone());

        let Some(tracks) = imp.tracks.borrow().clone() else {
            return;
        };
        tracks.remove_all();

        // Reading the tags of the tracks takes a moment, so we do it a few
        // at a time in the main loop, like when adding tracks to the playlist
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        let mut files = files.into_iter();
        glib::idle_add_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                if this.imp().generation.get() != generation {
                    return glib::ControlFlow::Break;
                }

                let batch: Vec<gio::File> = files.by_ref().take(4).collect();
                if batch.is_empty() {
                    return glib::ControlFlow::Break;
                }

                let batch: Vec<Track> = batch
                    .iter()
                    .filter_map(|f| Track::from_uri(f.uri().as_str()).ok())
                    .collect();
                tracks.extend_from_slice(&batch);

                // The list keeps its first track at the top as tracks are
                // added, which hides the header of its work above it
                if let Some(adjustment) = this.imp().tracks_view.vadjustment() {
                    adjustment.set_value(0.0);
                }

                glib::ControlFlow::Continue
            }
        ));
    }
}
