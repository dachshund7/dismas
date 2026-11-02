// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The cover of an album over its title and artist, in a box that
// highlights the album when it's selected in selection mode, and has the
// item it's in tell screen readers whether it's selected. The cover says
// how many of the album's tracks are taken from it, when it's only some of
// them, and with a heart, whether the album is a favorite; among the covers
// of the recordings of a work, whether the recording is one

use std::{
    cell::{Cell, OnceCell, RefCell},
    path::PathBuf,
    sync::Arc,
};

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::{
    i18n::{i18n, i18n_f},
    library::Album,
    play_stats::FavoriteRecording,
    square_bin::SquareBin,
    utils,
};

// The recording of a work whose tracks a cell takes from its album, among
// the covers of the recordings of a work, with the work's name
#[derive(Clone, Debug)]
pub(crate) struct CellWork {
    // The key of the work whose covers it's among, which finds its row
    pub(crate) key: String,
    pub(crate) name: String,
    // The recording, which can be marked as a favorite, and its tracks
    pub(crate) recording: FavoriteRecording,
    pub(crate) paths: Arc<[PathBuf]>,
}

// The smallest and largest sizes of a cover at a step of the zoom, 0 for
// the normal size
pub(crate) type CoverSizes = fn(i32) -> (i32, i32);

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::AlbumCell)]
    pub(crate) struct AlbumCell {
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub(crate) selected: Cell<bool>,
        #[property(get, set = Self::set_selection_mode, explicit_notify)]
        pub(crate) selection_mode: Cell<bool>,
        // How many of the album's tracks are taken from it, and how many it
        // has; 0 for none when they're all taken
        #[property(get, set = Self::set_matching)]
        pub(crate) matching: Cell<u32>,
        #[property(get, set = Self::set_total)]
        pub(crate) total: Cell<u32>,
        // Whether the heart shows: in the grid, for a favorite album, and
        // among the covers of a work's recordings, for a favorite recording
        #[property(get, set = Self::set_favorite)]
        pub(crate) favorite: Cell<bool>,
        // The step the covers are zoomed to, 0 for the normal size
        #[property(get, set = Self::set_zoom)]
        pub(crate) zoom: Cell<i32>,
        // The sizes the cover grows between at each step
        pub(crate) cover_sizes: Cell<Option<super::CoverSizes>>,
        pub(crate) square: OnceCell<SquareBin>,
        pub(crate) picture: gtk::Picture,
        pub(crate) badge: gtk::Label,
        pub(crate) heart: gtk::Image,
        pub(crate) work: RefCell<Option<CellWork>>,
        // The album the cell shows, and the item of the grid it's in
        pub(crate) album: RefCell<Option<Album>>,
        pub(crate) list_item: glib::WeakRef<gtk::ListItem>,
        pub(crate) title: gtk::Label,
        pub(crate) artist: gtk::Label,
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

        fn set_favorite(&self, favorite: bool) {
            self.favorite.set(favorite);
            self.heart.set_visible(favorite);
        }

        fn set_zoom(&self, zoom: i32) {
            self.zoom.set(zoom);
            if let (Some(sizes), Some(square)) = (self.cover_sizes.get(), self.square.get()) {
                let (min, max) = sizes(zoom);
                square.set_size_range(min, max);
            }
        }
    }
}

glib::wrapper! {
    pub(crate) struct AlbumCell(ObjectSubclass<imp::AlbumCell>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl AlbumCell {
    // The cover grows with the width it's given, between the sizes that
    // `cover_sizes` gives for the step the covers are zoomed to
    pub(crate) fn new(cover_sizes: CoverSizes) -> Self {
        let cell: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("spacing", 3)
            .build();
        cell.add_css_class("album-cell");
        let imp = cell.imp();

        let picture = &imp.picture;
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
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
        overlay.add_overlay(picture);

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
        heart.add_css_class("favorite");
        heart.update_property(&[gtk::accessible::Property::Label(&i18n("Favorite"))]);
        overlay.add_overlay(heart);

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
        // So is the line under it, which among the covers of a work names
        // the performers after what an arrangement is
        artist.set_has_tooltip(true);
        artist.connect_query_tooltip(|label, _, _, _, tooltip| {
            let text = label.label();
            tooltip.set_text(Some(&text));
            !text.is_empty()
        });

        let (min, max) = cover_sizes(0);
        let square = SquareBin::new(&overlay, min, max);
        cell.append(&square);
        imp.cover_sizes.set(Some(cover_sizes));
        let _ = imp.square.set(square);
        cell.append(title);
        cell.append(artist);
        cell.connect_map(Self::update_selected_state);
        cell
    }

    pub(crate) fn picture(&self) -> &gtk::Picture {
        &self.imp().picture
    }

    // Shows the title and the artist of an album, and no cover until it's
    // loaded
    pub(crate) fn show_album(&self, album: &Album) {
        let imp = self.imp();
        imp.title.set_label(&album.title());
        imp.artist.set_label(&album.artist());
        self.picture().set_paintable(gtk::gdk::Paintable::NONE);
        imp.album.replace(Some(album.clone()));
    }

    pub(crate) fn album(&self) -> Option<Album> {
        self.imp().album.borrow().clone()
    }

    // The line under the title, in place of the artist
    pub(crate) fn set_subtitle(&self, subtitle: &str) {
        self.imp().artist.set_label(subtitle);
    }

    pub(crate) fn work(&self) -> Option<CellWork> {
        self.imp().work.borrow().clone()
    }

    pub(crate) fn set_work(&self, work: Option<CellWork>) {
        self.imp().work.replace(work);
    }

    pub(crate) fn set_list_item(&self, item: &gtk::ListItem) {
        self.imp().list_item.set(Some(item));
        crate::utils::keep_selected_state(
            item,
            self,
            Self::selection_mode,
            Self::update_selected_state,
        );
    }

    // The position in the grid of the album the cell shows
    pub(crate) fn position(&self) -> Option<u32> {
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
    // selection mode; otherwise, it's not, as the grid itself says (see
    // `utils::keep_selected_state`)
    fn update_selected_state(&self) {
        let Some(item) = utils::list_item_of(self) else {
            return;
        };
        let imp = self.imp();
        let selected = imp.selection_mode.get() && imp.selected.get();
        item.update_state(&[gtk::accessible::State::Selected(Some(selected))]);
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
