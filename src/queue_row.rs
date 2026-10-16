// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::subclass::prelude::*;
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib, prelude::*};

use crate::{application::Application, audio::Track, cover_picture::CoverPicture, i18n::pi18n};

mod imp {
    use std::sync::LazyLock;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::QueueRow)]
    #[template(resource = "/io/bassi/Amberol/queue-row.ui")]
    pub struct QueueRow {
        // Template widgets
        #[template_child]
        pub row_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub track_cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        #[property(
            name = "track-cover",
            get = |imp: &Self| imp.track_cover_image.cover(),
            set = Self::set_track_cover,
            nullable,
            type = Option<gdk::Texture>,
        )]
        pub track_cover_image: TemplateChild<CoverPicture>,
        #[template_child]
        #[property(
            name = "track-title",
            get = |imp: &Self| imp.track_title_label.text().unwrap_or_default().into(),
            set = Self::set_track_title,
            type = String,
        )]
        pub track_title_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        #[property(
            name = "track-artist",
            get = |imp: &Self| imp.track_artist_label.text().unwrap_or_default().into(),
            set = Self::set_track_artist,
            type = String,
        )]
        pub track_artist_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub track_playing_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub selection_title_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub selection_artist_label: TemplateChild<gtk::Inscription>,
        // The check button notifies changes to the selection
        #[template_child]
        #[property(
            name = "selected",
            get = |imp: &Self| imp.selected_button.is_active(),
            set = |imp: &Self, selected: bool| imp.selected_button.set_active(selected),
            explicit_notify,
            type = bool,
        )]
        pub selected_button: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub selection_playing_image: TemplateChild<gtk::Image>,

        #[property(get, set, nullable)]
        pub track: RefCell<Option<Track>>,
        #[property(get, set = Self::set_playing, explicit_notify)]
        pub playing: Cell<bool>,
        // Whether the player is paused, for the icon of the current track
        #[property(get, set = Self::set_paused, explicit_notify)]
        pub paused: Cell<bool>,
        #[property(get, set = Self::set_selection_mode, explicit_notify)]
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

    #[glib::derived_properties]
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

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    // A right click or long press, at the given point
                    Signal::builder("menu-requested")
                        .param_types([f64::static_type(), f64::static_type()])
                        .build(),
                    // A Ctrl+click, to select the track
                    Signal::builder("select-requested").build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for QueueRow {}

    impl QueueRow {
        fn set_playing(&self, playing: bool) {
            if playing != self.playing.replace(playing) {
                let obj = self.obj();
                obj.update_mode();
                obj.notify_playing();
            }
        }

        fn set_paused(&self, paused: bool) {
            if paused != self.paused.replace(paused) {
                let obj = self.obj();
                obj.update_playing_icon();
                obj.notify_paused();
            }
        }

        fn set_selection_mode(&self, selection_mode: bool) {
            if selection_mode != self.selection_mode.replace(selection_mode) {
                let obj = self.obj();
                obj.update_mode();
                obj.notify_selection_mode();
            }
        }

        fn set_track_title(&self, title: String) {
            self.track_title_label.set_text(Some(&title));
            self.selection_title_label.set_text(Some(&title));
            self.obj().update_tooltip();
        }

        fn set_track_artist(&self, artist: String) {
            self.track_artist_label.set_text(Some(&artist));
            self.selection_artist_label.set_text(Some(&artist));
            self.obj().update_tooltip();
        }

        fn set_track_cover(&self, cover: Option<gdk::Texture>) {
            if let Some(texture) = cover {
                self.track_cover_image.set_cover(Some(&texture));
                self.track_cover_stack.set_visible_child_name("cover");
            } else {
                self.track_cover_image.set_cover(None::<&gdk::Texture>);
                self.track_cover_stack.set_visible_child_name("no-cover");
            }
        }
    }
}

glib::wrapper! {
    pub struct QueueRow(ObjectSubclass<imp::QueueRow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for QueueRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl QueueRow {
    // Shows the track of the list item, like in the playlist
    pub fn bind_list_item(&self, list_item: &gtk::ListItem) {
        list_item
            .bind_property("item", self, "track")
            .sync_create()
            .build();

        let item = list_item.property_expression("item");
        item.chain_property::<Track>("row-subtitle")
            .bind(self, "track-artist", gtk::Widget::NONE);
        item.chain_property::<Track>("row-title")
            .bind(self, "track-title", gtk::Widget::NONE);
        item.chain_property::<Track>("cover")
            .bind(self, "track-cover", gtk::Widget::NONE);
        item.chain_property::<Track>("playing")
            .bind(self, "playing", gtk::Widget::NONE);
        item.chain_property::<Track>("selected")
            .bind(self, "selected", gtk::Widget::NONE);

        if let Some(app) = gio::Application::default().and_downcast::<Application>() {
            app.player()
                .state()
                .bind_property("playing", self, "paused")
                .invert_boolean()
                .sync_create()
                .build();
        }
    }

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
                this.notify_selected();
            }
        ));

        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        click.connect_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.emit_by_name::<()>("menu-requested", &[&x, &y]);
            }
        ));
        self.add_controller(click);

        let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
        long_press.connect_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.emit_by_name::<()>("menu-requested", &[&x, &y]);
            }
        ));
        self.add_controller(long_press);

        let ctrl_click = gtk::GestureClick::new();
        ctrl_click.connect_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, _, _| {
                if !gesture
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                {
                    return;
                }
                // Instead of playing the track
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.emit_by_name::<()>("select-requested", &[]);
            }
        ));
        self.add_controller(ctrl_click);
    }

    pub fn connect_select_requested<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "select-requested",
            false,
            glib::closure_local!(move |row: &Self| f(row)),
        )
    }

    pub fn connect_menu_requested<F: Fn(&Self, f64, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "menu-requested",
            false,
            glib::closure_local!(move |row: &Self, x: f64, y: f64| f(row, x, y)),
        )
    }

    // The current track shows a pause icon while the player is paused
    fn update_playing_icon(&self) {
        let imp = self.imp();
        let (icon, label) = if imp.paused.get() {
            ("media-playback-pause-symbolic", pi18n("a11y", "Paused"))
        } else {
            ("media-playback-start-symbolic", pi18n("a11y", "Currently playing"))
        };
        for image in [&*imp.track_playing_image, &*imp.selection_playing_image] {
            image.set_icon_name(Some(icon));
        }
        imp.track_playing_image
            .update_property(&[gtk::accessible::Property::Label(&label)]);
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

    // Long titles get truncated in the playlist, so we show the full
    // text of the row on hover; for movements, that includes the work from
    // the header of their section, which may be scrolled away
    fn update_tooltip(&self) {
        let imp = self.imp();
        let mut lines = vec![
            imp.track_title_label.text().unwrap_or_default().to_string(),
            imp.track_artist_label
                .text()
                .unwrap_or_default()
                .to_string(),
        ];

        if let Some(work) = self.track().and_then(|t| t.movement_work()) {
            lines.insert(0, work);
        }

        let tooltip = lines
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        self.set_tooltip_text(Some(tooltip.as_str()).filter(|s| !s.is_empty()));
    }
}
