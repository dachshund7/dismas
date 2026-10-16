// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib, prelude::*};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/playlist-view.ui")]
    pub struct PlaylistView {
        #[template_child]
        pub back_button: TemplateChild<gtk::Button>,
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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistView {
        const NAME: &'static str = "AmberolPlaylistView";
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
    pub fn new() -> Self {
        Self::default()
    }

    fn setup_searchbar(&self) {
        let entry = self.imp().queue_searchentry.get();
        self.imp().queue_searchbar.connect_entry(&entry);
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
}
