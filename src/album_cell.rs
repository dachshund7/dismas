// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The content of a cell of the album grid: a box that highlights the album
// when it's selected in selection mode, and tells screen readers whether
// it's selected

use std::cell::Cell;

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::AlbumCell)]
    pub struct AlbumCell {
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub selected: Cell<bool>,
        #[property(get, set = Self::set_selection_mode, explicit_notify)]
        pub selection_mode: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumCell {
        const NAME: &'static str = "AmberolAlbumCell";
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

impl Default for AlbumCell {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AlbumCell {
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
