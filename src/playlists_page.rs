// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The list of playlists, with the queue at the top; activating one
// selects it, through the `win.select-playlist` action

use adw::{prelude::*, subclass::prelude::*};
use gtk::{CompositeTemplate, glib};

use crate::i18n::i18n;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/playlists-page.ui")]
    pub struct PlaylistsPage {
        #[template_child]
        pub playlists_list: TemplateChild<gtk::ListBox>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistsPage {
        const NAME: &'static str = "AmberolPlaylistsPage";
        type Type = super::PlaylistsPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            crate::page_switcher::PageSwitcher::static_type();
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("playlistspage");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PlaylistsPage {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PlaylistsPage {}
}

glib::wrapper! {
    pub struct PlaylistsPage(ObjectSubclass<imp::PlaylistsPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PlaylistsPage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PlaylistsPage {
    pub fn new() -> Self {
        Self::default()
    }

    // Shows the queue, then the saved playlists; `selected` is the name
    // of the selected one, empty for the queue
    pub fn set_playlists(&self, names: &[String], selected: &str) {
        let list = self.imp().playlists_list.get();
        list.remove_all();

        list.append(&playlist_row(&i18n("Queue"), "", selected.is_empty()));
        for name in names {
            list.append(&playlist_row(name, name, name == selected));
        }
    }
}

fn playlist_row(title: &str, name: &str, selected: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        // Names can have characters that mean something in markup
        .use_markup(false)
        .activatable(true)
        .action_name("win.select-playlist")
        .action_target(&name.to_variant())
        .build();

    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.set_visible(selected);
    check.set_tooltip_text(Some(&i18n("Selected")));
    row.add_suffix(&check);
    let checked = if selected {
        gtk::AccessibleTristate::True
    } else {
        gtk::AccessibleTristate::False
    };
    row.upcast_ref::<gtk::Widget>()
        .update_state(&[gtk::accessible::State::Checked(checked)]);

    row
}
