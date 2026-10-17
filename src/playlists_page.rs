// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The list of playlists, with the queue at the top; activating one
// selects it, through the `win.select-playlist` action

use adw::{prelude::*, subclass::prelude::*};
use gtk::{CompositeTemplate, gio, glib};

use crate::i18n::{i18n, i18n_k};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/playlists-page.ui")]
    pub struct PlaylistsPage {
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
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
        fn constructed(&self) {
            self.parent_constructed();

            self.menu_button
                .set_menu_model(Some(&crate::utils::primary_menu()));
            self.menu_button.set_primary(true);
        }

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
            let row = playlist_row(name, name, name == selected);
            row.add_suffix(&playlist_menu_button(name));
            list.append(&row);
        }
    }
}

// The queue can't be renamed or deleted, only the saved playlists
fn playlist_menu_button(name: &str) -> gtk::MenuButton {
    let target = name.to_variant();
    let menu = gio::Menu::new();
    let rename = gio::MenuItem::new(Some(&i18n("_Rename…")), None);
    rename.set_action_and_target_value(Some("win.rename-playlist"), Some(&target));
    menu.append_item(&rename);
    let delete = gio::MenuItem::new(Some(&i18n("_Delete…")), None);
    delete.set_action_and_target_value(Some("win.delete-playlist"), Some(&target));
    menu.append_item(&delete);

    let button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .tooltip_text(i18n("More Options"))
        .css_classes(["flat"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&i18n_k(
        // Translators: `{playlist}` must be left unmodified; it is
        // expanded to the name of a playlist
        "Options for {playlist}",
        &[("playlist", name)],
    ))]);
    button
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

    // Hidden rather than left out on the other rows, so that the names
    // line up
    let caret = gtk::Image::from_icon_name("go-next-symbolic");
    caret.set_opacity(if selected { 1.0 } else { 0.0 });
    if selected {
        caret.set_tooltip_text(Some(&i18n("Selected")));
    }
    row.add_prefix(&caret);
    let checked = if selected {
        gtk::AccessibleTristate::True
    } else {
        gtk::AccessibleTristate::False
    };
    row.upcast_ref::<gtk::Widget>()
        .update_state(&[gtk::accessible::State::Checked(checked)]);

    row
}
