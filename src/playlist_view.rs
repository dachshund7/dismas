// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, gdk, gio, glib, prelude::*};

// How close to the top or bottom of the queue a dragged track scrolls it,
// and how fast at most, in pixels per frame
const AUTOSCROLL_EDGE: f64 = 48.0;
const AUTOSCROLL_MAX_SPEED: f64 = 12.0;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/playlist-view.ui")]
    pub struct PlaylistView {
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub title_menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub queue_scrolled: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub queue_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub queue_length_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub queue_actionbar: TemplateChild<gtk::ActionBar>,
        #[template_child]
        pub queue_select_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub queue_remove_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub queue_selected_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub queue_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub queue_searchbar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub queue_searchentry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub queue_no_results: TemplateChild<adw::StatusPage>,

        // While a track is dragged near an edge of the queue, the pixels to
        // scroll it by on each frame
        pub autoscroll_speed: Cell<f64>,
        pub autoscroll_tick: RefCell<Option<gtk::TickCallbackId>>,
        // The drag that is scrolling the queue, to stop when it ends
        pub autoscroll_drag: glib::WeakRef<gdk::Drag>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistView {
        const NAME: &'static str = "DismasPlaylistView";
        type Type = super::PlaylistView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("playlistview");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PlaylistView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.obj().setup_searchbar();
            self.obj().setup_autoscroll();
            self.obj().setup_title_menu();
            // It names the selected playlist
            crate::utils::wrap_tooltip(&*self.back_button);

            // Strict centering sizes both sides like the wider one, which
            // with the back button would leave the title little room
            self.back_button.connect_visible_notify(clone!(
                #[weak(rename_to = header_bar)]
                self.header_bar.get(),
                move |button| {
                    header_bar.set_centering_policy(if button.is_visible() {
                        adw::CenteringPolicy::Loose
                    } else {
                        adw::CenteringPolicy::Strict
                    });
                }
            ));
        }
    }

    impl WidgetImpl for PlaylistView {}
}

glib::wrapper! {
    pub struct PlaylistView(ObjectSubclass<imp::PlaylistView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PlaylistView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PlaylistView {
    // A track or files dragged near the top or bottom of the queue scroll
    // it, faster closer to the edge. GTK's own scrolling of lists while
    // dragging is taken out: it only stops when the drag leaves the list,
    // which it doesn't after a drop on X11, so a drop near the edge kept
    // pushing the list there for good
    fn setup_autoscroll(&self) {
        let queue_view = &*self.imp().queue_view;
        let gtk_autoscroll: Vec<gtk::EventController> = queue_view
            .observe_controllers()
            .iter::<glib::Object>()
            .flatten()
            .filter_map(|c| c.downcast::<gtk::DropControllerMotion>().ok())
            .map(Cast::upcast)
            .collect();
        for controller in &gtk_autoscroll {
            queue_view.remove_controller(controller);
        }

        let motion = gtk::DropControllerMotion::new();
        motion.connect_motion(clone!(
            #[weak(rename_to = this)]
            self,
            move |motion, _, y| {
                this.watch_drag_end(motion);
                let height = f64::from(this.imp().queue_scrolled.height());
                let speed = if y < AUTOSCROLL_EDGE {
                    -(AUTOSCROLL_EDGE - y) / AUTOSCROLL_EDGE
                } else if y > height - AUTOSCROLL_EDGE {
                    (y - (height - AUTOSCROLL_EDGE)) / AUTOSCROLL_EDGE
                } else {
                    0.0
                };
                this.set_autoscroll_speed(speed.clamp(-1.0, 1.0) * AUTOSCROLL_MAX_SPEED);
            }
        ));
        motion.connect_leave(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.set_autoscroll_speed(0.0)
        ));
        self.imp().queue_scrolled.add_controller(motion);
    }

    // The list isn't told that the drag left it when the track is dropped,
    // so the scrolling also stops when the drag ends; tracks are only
    // dragged within Dismas, so the drag is known. The drag of files from
    // another app isn't, and the window's drop target stops it instead
    fn watch_drag_end(&self, motion: &gtk::DropControllerMotion) {
        let Some(drag) = motion.drop().and_then(|drop| drop.drag()) else {
            return;
        };
        let imp = self.imp();
        if imp.autoscroll_drag.upgrade().as_ref() == Some(&drag) {
            return;
        }
        imp.autoscroll_drag.set(Some(&drag));

        let stop = clone!(
            #[weak(rename_to = this)]
            self,
            move || this.set_autoscroll_speed(0.0)
        );
        drag.connect_drop_performed(clone!(
            #[strong]
            stop,
            move |_| stop()
        ));
        drag.connect_dnd_finished(clone!(
            #[strong]
            stop,
            move |_| stop()
        ));
        drag.connect_cancel(move |_, _| stop());
    }

    // For drags from other apps, whose end isn't known
    pub fn stop_autoscroll(&self) {
        self.set_autoscroll_speed(0.0);
    }

    fn set_autoscroll_speed(&self, speed: f64) {
        let imp = self.imp();
        imp.autoscroll_speed.set(speed);

        if speed == 0.0 {
            if let Some(tick) = imp.autoscroll_tick.take() {
                tick.remove();
            }
        } else if imp.autoscroll_tick.borrow().is_none() {
            let tick = imp.queue_scrolled.add_tick_callback(clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move |scrolled, _| {
                    let adjustment = scrolled.vadjustment();
                    adjustment.set_value(adjustment.value() + this.imp().autoscroll_speed.get());
                    glib::ControlFlow::Continue
                }
            ));
            imp.autoscroll_tick.replace(Some(tick));
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    // The time left under the title opens its menu too, as part of the
    // title, without making the button as wide as it, which would cut it
    fn setup_title_menu(&self) {
        let click = gtk::GestureClick::new();
        click.connect_released(clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.imp().title_menu_button.popup();
            }
        ));
        self.imp().queue_length_label.add_controller(click);
    }

    fn setup_searchbar(&self) {
        let entry = self.imp().queue_searchentry.get();
        self.imp().queue_searchbar.connect_entry(&entry);
        crate::utils::focus_on_clear(&entry);
    }

    // The name of the selected playlist
    pub fn set_title(&self, title: &str) {
        self.imp().title_label.set_label(title);
    }

    // The playlists the title switches to
    pub fn set_playlists_menu(&self, menu: &gio::MenuModel) {
        self.imp().title_menu_button.set_menu_model(Some(menu));
    }

    pub fn back_button(&self) -> gtk::Button {
        self.imp().back_button.get()
    }

    pub fn queue_actionbar(&self) -> gtk::ActionBar {
        self.imp().queue_actionbar.get()
    }

    pub fn queue_remove_button(&self) -> gtk::Button {
        self.imp().queue_remove_button.get()
    }

    pub fn queue_select_all_button(&self) -> gtk::Button {
        self.imp().queue_select_all_button.get()
    }

    pub fn queue_selected_label(&self) -> gtk::Label {
        self.imp().queue_selected_label.get()
    }

    pub fn queue_scrolled(&self) -> gtk::ScrolledWindow {
        self.imp().queue_scrolled.get()
    }

    pub fn queue_view(&self) -> gtk::ListView {
        self.imp().queue_view.get()
    }

    pub fn queue_length_label(&self) -> gtk::Label {
        self.imp().queue_length_label.get()
    }

    pub fn queue_searchbar(&self) -> gtk::SearchBar {
        self.imp().queue_searchbar.get()
    }

    pub fn queue_searchentry(&self) -> gtk::SearchEntry {
        self.imp().queue_searchentry.get()
    }

    pub fn begin_loading(&self) {
        self.imp().queue_progress.set_fraction(0.0);
        self.imp().queue_progress.set_visible(true);
    }

    pub fn end_loading(&self) {
        self.imp().queue_progress.set_visible(false);
    }

    pub fn update_loading(&self, cur: u32, max: u32) {
        let step = f64::from(cur) / f64::from(max);
        self.imp().queue_progress.set_fraction(step);
    }

    pub fn set_search(&self, search: bool) {
        self.imp().queue_searchbar.set_search_mode(search);
    }

    // Says that the search found nothing, in place of an empty list
    pub fn set_no_results(&self, no_results: bool) {
        self.imp().queue_no_results.set_visible(no_results);
    }
}
