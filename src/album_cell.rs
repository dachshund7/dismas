// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The content of a cell of the album grid: the cover over the album's title
// and artist, in a box that highlights the album when it's selected in
// selection mode, and tells screen readers whether it's selected

use std::cell::{Cell, OnceCell, RefCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::library::Album;

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::AlbumCell)]
    pub struct AlbumCell {
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub selected: Cell<bool>,
        #[property(get, set = Self::set_selection_mode, explicit_notify)]
        pub selection_mode: Cell<bool>,
        pub picture: OnceCell<gtk::Picture>,
        // The album the cell shows, and the item of the grid it's in
        pub album: RefCell<Option<Album>>,
        pub list_item: glib::WeakRef<gtk::ListItem>,
        pub title: gtk::Label,
        pub artist: gtk::Label,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumCell {
        const NAME: &'static str = "DismasAlbumCell";
        type Type = super::AlbumCell;
        type ParentType = gtk::Box;
    }

    #[glib::derived_properties]
    impl ObjectImpl for AlbumCell {}

    impl WidgetImpl for AlbumCell {}

    impl BoxImpl for AlbumCell {}

    impl AlbumCell {
        fn set_selected(&self, selected: bool) {
            if selected != self.selected.replace(selected) {
                let obj = self.obj();
                obj.update_selected();
                obj.notify_selected();
            }
        }

        fn set_selection_mode(&self, selection_mode: bool) {
            if selection_mode != self.selection_mode.replace(selection_mode) {
                let obj = self.obj();
                obj.update_selected();
                obj.notify_selection_mode();
            }
        }
    }
}

glib::wrapper! {
    pub struct AlbumCell(ObjectSubclass<imp::AlbumCell>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl AlbumCell {
    // The cover has the picture the album's cover is loaded into
    pub fn new(cover: &impl IsA<gtk::Widget>, picture: &gtk::Picture) -> Self {
        let cell: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("spacing", 3)
            .build();
        cell.add_css_class("album-cell");
        let imp = cell.imp();
        imp.picture.set(picture.clone()).unwrap();

        let title = &imp.title;
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(1);
        title.set_hexpand(true);
        title.add_css_class("album-title");
        // The title is often ellipsized, so the tooltip shows all of it;
        // setting the tooltip text on each bind is slow, as it also updates
        // the accessible description, so we only fill it in when it's shown
        title.set_has_tooltip(true);
        title.connect_query_tooltip(|label, _, _, _, tooltip| {
            tooltip.set_text(Some(&label.label()));
            true
        });
        let artist = &imp.artist;
        artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
        artist.set_max_width_chars(1);
        artist.set_hexpand(true);
        artist.add_css_class("caption");
        artist.add_css_class("dim-label");

        cell.append(cover);
        cell.append(title);
        cell.append(artist);
        cell
    }

    pub fn picture(&self) -> &gtk::Picture {
        self.imp().picture.get().unwrap()
    }

    // Shows the title and the artist of an album, and no cover until it's
    // loaded
    pub fn show_album(&self, album: &Album) {
        let imp = self.imp();
        imp.title.set_label(&album.title());
        imp.artist.set_label(&album.artist());
        self.picture().set_paintable(gtk::gdk::Paintable::NONE);
        imp.album.replace(Some(album.clone()));
    }

    pub fn album(&self) -> Option<Album> {
        self.imp().album.borrow().clone()
    }

    pub fn set_list_item(&self, item: &gtk::ListItem) {
        self.imp().list_item.set(Some(item));
    }

    // The position in the grid of the album the cell shows
    pub fn position(&self) -> Option<u32> {
        let position = self.imp().list_item.upgrade()?.position();
        (position != gtk::INVALID_LIST_POSITION).then_some(position)
    }

    // Selected albums are highlighted in selection mode, in place of a
    // check button over the cover
    fn update_selected(&self) {
        let imp = self.imp();
        let selection_mode = imp.selection_mode.get();
        let highlighted = selection_mode && imp.selected.get();
        if highlighted {
            self.add_css_class("selected-item");
        } else {
            self.remove_css_class("selected-item");
        }
        if selection_mode {
            self.update_state(&[gtk::accessible::State::Selected(Some(highlighted))]);
        } else {
            self.reset_state(gtk::AccessibleState::Selected);
        }
    }
}
