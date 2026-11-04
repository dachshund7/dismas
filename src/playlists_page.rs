// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The list of playlists, with the queue at the top, and the smart
// playlists. Activating one shows its tracks in the pane, through the
// `win.show-playlist` and `win.show-smart-playlist` actions, without
// switching to it; its menu's Switch To switches, through
// `win.select-playlist`, as the menu of the pane's title does. The radio
// only shows which one is selected

use adw::{prelude::*, subclass::prelude::*};
use gtk::{CompositeTemplate, gdk, gio, glib};

use crate::{
    i18n::{i18n, i18n_k},
    window::SmartPlaylist,
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/playlists-page.ui")]
    pub(crate) struct PlaylistsPage {
        #[template_child]
        pub(crate) menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(crate) playlists_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub(crate) smart_group: TemplateChild<adw::PreferencesGroup>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistsPage {
        const NAME: &'static str = "DismasPlaylistsPage";
        type Type = super::PlaylistsPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            crate::fit_bin::FitBin::static_type();
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

            crate::utils::setup_primary_menu_button(&self.menu_button, None, None);
            // The Menu key opens the ⋯ menu of the row with the focus
            crate::utils::setup_menu_key(&*self.playlists_list, |row: &adw::ActionRow| {
                if let Some(button) = menu_button_in(row.upcast_ref())
                    && let Some(model) = button.menu_model()
                {
                    open_row_menu(row, &button, &model);
                }
            });
            for kind in SmartPlaylist::ALL {
                let row = adw::ActionRow::builder()
                    .title(kind.title())
                    .subtitle(kind.description())
                    .activatable(true)
                    .action_name("win.show-smart-playlist")
                    .action_target(&kind.name().to_variant())
                    .build();
                self.smart_group.add(&row);
            }
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
    pub(crate) struct PlaylistsPage(ObjectSubclass<imp::PlaylistsPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PlaylistsPage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PlaylistsPage {
    // Shows the queue, then the saved playlists; `selected` is the name
    // of the selected one, empty for the queue
    pub(crate) fn set_playlists(&self, names: &[String], selected: &str) {
        let list = self.imp().playlists_list.get();
        list.remove_all();

        // Radio buttons, since exactly one playlist is selected
        let group = gtk::CheckButton::new();
        let queue_row = playlist_row(&i18n("Queue"), "", selected.is_empty(), &group);
        queue_row.add_suffix(&playlist_menu_button(
            &i18n("Queue"),
            "",
            selected.is_empty(),
        ));
        list.append(&queue_row);
        for name in names {
            let row = playlist_row(name, name, name == selected, &group);
            row.add_suffix(&playlist_menu_button(name, name, name == selected));
            list.append(&row);
        }
    }
}

// The menu of a row opened with the Menu key: the ⋯ button's menu, over the
// button, but on the row, so that the focus comes back to the row once it
// closes, as from the menus of the other lists; the button's own popover
// would leave it on the button. The popover goes once the item's action ran,
// as it can take the row away (Switch To), see `utils::menu_popover()`
fn open_row_menu(row: &adw::ActionRow, button: &gtk::MenuButton, model: &gio::MenuModel) {
    let bounds = button.compute_bounds(row).map(|bounds| {
        gdk::Rectangle::new(
            bounds.x() as i32,
            bounds.y() as i32,
            bounds.width() as i32,
            bounds.height() as i32,
        )
    });
    crate::utils::menu_popover(model, row, bounds.as_ref()).popup();
}

// The ⋯ button among the suffixes of a row
fn menu_button_in(widget: &gtk::Widget) -> Option<gtk::MenuButton> {
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Ok(button) = widget.clone().downcast::<gtk::MenuButton>() {
            return Some(button);
        }
        if let Some(button) = menu_button_in(&widget) {
            return Some(button);
        }
        child = widget.next_sibling();
    }
    None
}

// The menu of a playlist; `name` is empty for the queue, which can't be
// renamed or deleted, only saved as a playlist or exported. One that isn't
// `selected` can be switched to
fn playlist_menu_button(title: &str, name: &str, selected: bool) -> gtk::MenuButton {
    let target = name.to_variant();
    let menu = gio::Menu::new();
    if !selected {
        let switch = gio::MenuItem::new(Some(&i18n("_Switch To")), None);
        switch.set_action_and_target_value(Some("win.select-playlist"), Some(&target));
        menu.append_item(&switch);
    }
    if name.is_empty() {
        menu.append(Some(&i18n("_Save as Playlist…")), Some("win.save-queue"));
    } else {
        let rename = gio::MenuItem::new(Some(&i18n("_Rename…")), None);
        rename.set_action_and_target_value(Some("win.rename-playlist"), Some(&target));
        menu.append_item(&rename);
    }
    let export = gio::MenuItem::new(Some(&i18n("_Export…")), None);
    export.set_action_and_target_value(Some("win.export-playlist"), Some(&target));
    menu.append_item(&export);
    if !name.is_empty() {
        let delete = gio::MenuItem::new(Some(&i18n("_Delete…")), None);
        delete.set_action_and_target_value(Some("win.delete-playlist"), Some(&target));
        menu.append_item(&delete);
    }

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
        &[("playlist", title)],
    ))]);
    button
}

fn playlist_row(
    title: &str,
    name: &str,
    selected: bool,
    group: &gtk::CheckButton,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        // Names can have characters that mean something in markup
        .use_markup(false)
        .activatable(true)
        .action_name("win.show-playlist")
        .action_target(&name.to_variant())
        .build();

    // Only showing which one is selected: activating the row shows its
    // tracks, so the row isn't checked itself, which would say that it
    // switches; the radio is, and the row's description says so. The
    // description is a label the row is described by, as the row
    // describes itself by its subtitle, which is empty, over a
    // description set on it
    let radio = gtk::CheckButton::builder()
        .group(group)
        .active(selected)
        .can_target(false)
        .can_focus(false)
        .valign(gtk::Align::Center)
        .build();
    radio.update_property(&[gtk::accessible::Property::Label(title)]);
    row.add_prefix(&radio);
    if selected {
        // Translators: said by screen readers of the playlist whose tracks
        // are in the player
        let description = i18n("Selected");
        let widget = row.upcast_ref::<gtk::Widget>();
        widget.reset_relation(gtk::AccessibleRelation::DescribedBy);
        widget.update_property(&[gtk::accessible::Property::Description(&description)]);
    }

    row
}
