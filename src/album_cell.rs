// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The cover of an album over its title and artist, in a box that
// highlights the album when it's selected in selection mode, and has the
// item it's in tell screen readers whether it's selected. The cover says
// how many of the album's tracks are taken from it, when it's only some of
// them, and among the covers of the albums of a work, whether one of its
// recordings there is a favorite

use std::cell::{Cell, OnceCell, RefCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::{
    i18n::{i18n, i18n_f},
    library::Album,
    play_stats::FavoriteRecording,
    square_bin::SquareBin,
};

// The work whose tracks a cell takes from its album, among the covers of the
// albums of a work, with its name and its recordings on the album
#[derive(Clone, Debug)]
pub struct CellWork {
    pub key: String,
    pub name: String,
    pub recordings: Vec<FavoriteRecording>,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::AlbumCell)]
    pub struct AlbumCell {
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub selected: Cell<bool>,
        #[property(get, set = Self::set_selection_mode, explicit_notify)]
        pub selection_mode: Cell<bool>,
        // How many of the album's tracks are taken from it, and how many it
        // has; 0 for none when they're all taken
        #[property(get, set = Self::set_matching)]
        pub matching: Cell<u32>,
        #[property(get, set = Self::set_total)]
        pub total: Cell<u32>,
        pub picture: OnceCell<gtk::Picture>,
        pub badge: gtk::Label,
        pub heart: gtk::Image,
        pub work: RefCell<Option<CellWork>>,
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

        fn set_matching(&self, matching: u32) {
            self.matching.set(matching);
            self.obj().update_badge();
        }

        fn set_total(&self, total: u32) {
            self.total.set(total);
            self.obj().update_badge();
        }
    }
}

glib::wrapper! {
    pub struct AlbumCell(ObjectSubclass<imp::AlbumCell>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl AlbumCell {
    // The cover grows with the width it's given, between these sizes
    pub fn new(min_cover_size: i32, max_cover_size: i32) -> Self {
        let cell: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("spacing", 3)
            .build();
        cell.add_css_class("album-cell");
        let imp = cell.imp();

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .hexpand(true)
            .vexpand(true)
            .build();
        let placeholder = gtk::Image::builder()
            .icon_name("folder-music-symbolic")
            .pixel_size(48)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["dim-label"])
            .build();
        let overlay = gtk::Overlay::builder()
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["album-cover"])
            .child(&placeholder)
            .build();
        overlay.add_overlay(&picture);

        // The number of the tracks taken and of all of them, e.g. "9 of 12"
        let badge = &imp.badge;
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::End);
        badge.set_margin_end(6);
        badge.set_margin_bottom(6);
        badge.set_can_target(false);
        badge.set_visible(false);
        badge.add_css_class("match-count");
        overlay.add_overlay(badge);
        let heart = &imp.heart;
        heart.set_icon_name(Some("heart-filled-symbolic"));
        heart.set_halign(gtk::Align::Start);
        heart.set_valign(gtk::Align::Start);
        heart.set_margin_start(6);
        heart.set_margin_top(6);
        heart.set_can_target(false);
        heart.set_visible(false);
        heart.add_css_class("favorite-recording");
        heart.update_property(&[gtk::accessible::Property::Label(&i18n("Favorite"))]);
        overlay.add_overlay(heart);
        imp.picture.set(picture).unwrap();

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

        cell.append(&SquareBin::new(&overlay, min_cover_size, max_cover_size));
        cell.append(title);
        cell.append(artist);
        cell.connect_map(Self::update_selected_state);
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

    // The line under the title, in place of the artist
    pub fn set_subtitle(&self, subtitle: &str) {
        self.imp().artist.set_label(subtitle);
    }

    pub fn set_favorite(&self, favorite: bool) {
        self.imp().heart.set_visible(favorite);
    }

    pub fn work(&self) -> Option<CellWork> {
        self.imp().work.borrow().clone()
    }

    pub fn set_work(&self, work: Option<CellWork>) {
        self.imp().work.replace(work);
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
        let highlighted = imp.selection_mode.get() && imp.selected.get();
        if highlighted {
            self.add_css_class("selected-item");
        } else {
            self.remove_css_class("selected-item");
        }
        self.update_selected_state();
    }

    // Screen readers present the item of the grid or of the flow box that
    // the cell is in, so it says whether the album is selected, in
    // selection mode. It's only taken while the cell is on screen: a list
    // binds a cell before it takes the item it's in, which is still floating
    // then, and a reference to it would sink it, so that the list would lose
    // it when the reference is dropped
    fn update_selected_state(&self) {
        if !self.is_mapped() {
            return;
        }
        let Some(item) = self.parent() else {
            return;
        };
        let imp = self.imp();
        if imp.selection_mode.get() {
            item.update_state(&[gtk::accessible::State::Selected(Some(imp.selected.get()))]);
        } else {
            item.reset_state(gtk::AccessibleState::Selected);
        }
    }

    fn update_badge(&self) {
        let imp = self.imp();
        let (matching, total) = (imp.matching.get(), imp.total.get());
        let shown = matching > 0 && matching < total;
        if shown {
            imp.badge.set_label(&i18n_f(
                // Translators: The first '{}' is the number of tracks of an
                // album that match the search, and the second the number of
                // all of them; both must be left unmodified
                "{} of {}",
                &[&matching.to_string(), &total.to_string()],
            ));
        }
        imp.badge.set_visible(shown);
    }
}
