// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::subclass::prelude::*;
use glib::clone;
use gtk::{gdk, gio, glib, prelude::*, CompositeTemplate};

use crate::{audio::Track, cover_picture::CoverPicture};

mod imp {
    use glib::{ParamSpec, ParamSpecBoolean, ParamSpecObject, ParamSpecString, Value};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/queue-row.ui")]
    pub struct QueueRow {
        // Template widgets
        #[template_child]
        pub row_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub track_cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub track_cover_image: TemplateChild<CoverPicture>,
        #[template_child]
        pub track_title_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub track_artist_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub track_composer_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub track_playing_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub selection_title_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub selection_artist_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub selection_composer_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub selected_button: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub selection_playing_image: TemplateChild<gtk::Image>,

        pub track: RefCell<Option<Track>>,
        pub playing: Cell<bool>,
        pub selection_mode: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for QueueRow {
        const NAME: &'static str = "AmberolQueueRow";
        type Type = super::QueueRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BoxLayout>();
            klass.set_css_name("queuerow");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for QueueRow {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.obj().init_widgets();
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecObject::builder::<Track>("track").build(),
                    ParamSpecString::builder("track-artist").build(),
                    ParamSpecString::builder("track-title").build(),
                    ParamSpecString::builder("track-composer").build(),
                    ParamSpecObject::builder::<gdk::Texture>("track-cover").build(),
                    ParamSpecBoolean::builder("playing").build(),
                    ParamSpecBoolean::builder("selection-mode").build(),
                    ParamSpecBoolean::builder("selected").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &Value, pspec: &ParamSpec) {
            match pspec.name() {
                "track" => {
                    let track = value.get::<Option<Track>>().unwrap();
                    self.track.replace(track);
                }
                "track-artist" => {
                    let p = value.get::<&str>().expect("The value needs to be a string");
                    self.obj().set_track_artist(p);
                }
                "track-title" => {
                    let p = value.get::<&str>().expect("The value needs to be a string");
                    self.obj().set_track_title(p);
                }
                "track-composer" => {
                    let p = value.get::<&str>().expect("The value needs to be a string");
                    self.obj().set_track_composer(p);
                }
                "track-cover" => {
                    let p = value.get::<gdk::Texture>().ok();
                    self.obj().set_track_cover(p);
                }
                "playing" => {
                    let p = value
                        .get::<bool>()
                        .expect("The value needs to be a boolean");
                    self.obj().set_playing(p);
                }
                "selection-mode" => {
                    let p = value
                        .get::<bool>()
                        .expect("The value needs to be a boolean");
                    self.obj().set_selection_mode(p);
                }
                "selected" => {
                    let p = value
                        .get::<bool>()
                        .expect("The value needs to be a boolean");
                    self.selected_button.set_active(p);
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> Value {
            match pspec.name() {
                "track" => self.track.borrow().to_value(),
                "track-artist" => self.track_artist_label.text().to_value(),
                "track-title" => self.track_title_label.text().to_value(),
                "track-composer" => self.track_composer_label.text().to_value(),
                "track-cover" => self.track_cover_image.cover().to_value(),
                "playing" => self.playing.get().to_value(),
                "selection-mode" => self.selection_mode.get().to_value(),
                "selected" => self.selected_button.is_active().to_value(),
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for QueueRow {}
}

glib::wrapper! {
    pub struct QueueRow(ObjectSubclass<imp::QueueRow>)
        @extends gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible,
                    gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for QueueRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl QueueRow {
    pub fn new() -> Self {
        Self::default()
    }

    fn init_widgets(&self) {
        self.imp().selected_button.connect_active_notify(clone!(
            #[strong(rename_to = this)]
            self,
            move |button| {
                if let Some(ref track) = *this.imp().track.borrow() {
                    track.set_selected(button.is_active());
                }
                this.notify("selected");
            }
        ));
    }

    fn set_playing(&self, playing: bool) {
        if playing != self.imp().playing.replace(playing) {
            self.update_mode();
            self.notify("playing");
        }
    }

    fn set_selection_mode(&self, selection_mode: bool) {
        if selection_mode != self.imp().selection_mode.replace(selection_mode) {
            self.update_mode();
            self.notify("selection-mode");
        }
    }

    fn update_mode(&self) {
        let imp = self.imp();
        if imp.selection_mode.get() {
            imp.row_stack.set_visible_child_name("selection-mode");
            let opacity = if imp.playing.get() { 1.0 } else { 0.0 };
            imp.selection_playing_image.set_opacity(opacity);
        } else {
            imp.row_stack.set_visible_child_name("track-details");
            let opacity = if imp.playing.get() { 1.0 } else { 0.0 };
            imp.track_playing_image.set_opacity(opacity);
        }
    }

    fn set_track_title(&self, title: &str) {
        let imp = self.imp();
        imp.track_title_label.set_text(Some(title));
        imp.selection_title_label.set_text(Some(title));
        self.update_tooltip();
    }

    fn set_track_artist(&self, artist: &str) {
        let imp = self.imp();
        imp.track_artist_label.set_text(Some(artist));
        imp.selection_artist_label.set_text(Some(artist));
        self.update_tooltip();
    }

    // Only movements have a third line, with the composer, as the second
    // line shows the work instead of the artist
    fn set_track_composer(&self, composer: &str) {
        let imp = self.imp();
        imp.track_composer_label.set_text(Some(composer));
        imp.track_composer_label.set_visible(!composer.is_empty());
        imp.selection_composer_label.set_text(Some(composer));
        imp.selection_composer_label
            .set_visible(!composer.is_empty());
        self.update_tooltip();
    }

    // Long titles get truncated in the playlist, so we show the full
    // text of the row on hover
    fn update_tooltip(&self) {
        let imp = self.imp();
        let lines = [
            imp.track_title_label.text(),
            imp.track_artist_label.text(),
            imp.track_composer_label.text(),
        ];

        let tooltip = lines
            .iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.set_tooltip_text(Some(tooltip.as_str()).filter(|s| !s.is_empty()));
    }

    fn set_track_cover(&self, cover: Option<gdk::Texture>) {
        let imp = self.imp();
        if let Some(texture) = cover {
            imp.track_cover_image.set_cover(Some(&texture));
            imp.track_cover_stack.set_visible_child_name("cover");
        } else {
            imp.track_cover_image.set_cover(None);
            imp.track_cover_stack.set_visible_child_name("no-cover");
        }
    }

    pub fn track(&self) -> Option<Track> {
        self.imp().track.borrow().clone()
    }
}
