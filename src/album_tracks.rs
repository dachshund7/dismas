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
    library::{self, Album},
    queue_row::QueueRow,
    work_sections::{WorkSectionModel, work_header_factory},
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/album-tracks.ui")]
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
        factory.connect_setup(|_, item| {
            let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = QueueRow::default();
            row.bind_list_item(list_item);
            list_item.set_child(Some(&row));
        });

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

        // Clicking a track plays the album from there
        imp.tracks_view.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, pos| {
                if this.imp().add_button.is_sensitive() {
                    this.emit_by_name::<()>("play", &[&pos]);
                }
            }
        ));
    }

    // The files of the tracks, in order
    pub fn files(&self) -> Vec<gio::File> {
        self.imp().files.borrow().clone()
    }

    // Albums can't be added while tracks are being added to the playlist
    pub fn set_adding_enabled(&self, enabled: bool) {
        self.imp().play_button.set_sensitive(enabled);
        self.imp().add_button.set_sensitive(enabled);
    }

    // Shows the tracks of an album that contain every search word, or all
    // of them without any
    pub fn show_album(&self, album: &Album, search_words: &[String]) {
        let imp = self.imp();

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
