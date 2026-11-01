// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The row of a work in the list of works: a line with its name, then,
// dimmed, what it arranges and the collection it's in, a heart if it's a
// favorite, and the number of its recordings. Under the line, once the work
// opens, the covers of its recordings

use super::{works::CoverRef, *};
use crate::reveal_bin::RevealBin;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct WorkRow {
        pub line: gtk::Box,
        pub name: gtk::Label,
        pub heart: gtk::Image,
        pub count: gtk::Label,
        // Made when the work first opens
        pub covers: RefCell<Option<RevealBin>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WorkRow {
        const NAME: &'static str = "DismasWorkRow";
        type Type = super::WorkRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for WorkRow {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                // The menu of the work, at a point of the line
                vec![
                    Signal::builder("menu-requested")
                        .param_types([f64::static_type(), f64::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for WorkRow {}
    impl BoxImpl for WorkRow {}
}

glib::wrapper! {
    pub struct WorkRow(ObjectSubclass<imp::WorkRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl WorkRow {
    pub fn new() -> Self {
        // Set, so that the line's expanding doesn't spread to the list
        let row: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("vexpand", false)
            .build();
        let imp = row.imp();
        fill_name_count_line(&imp.line, &imp.name, &imp.count);
        let heart = &imp.heart;
        heart.set_icon_name(Some("heart-filled-symbolic"));
        heart.add_css_class("favorite-work");
        heart.set_visible(false);
        heart.update_property(&[gtk::accessible::Property::Label(&i18n("Favorite"))]);
        imp.line.insert_child_after(heart, Some(&imp.name));
        row.append(&imp.line);

        // The menu, with a right click or a long press
        utils::setup_context_menu(
            &imp.line,
            clone!(
                #[weak]
                row,
                move |x, y| row.emit_by_name::<()>("menu-requested", &[&x, &y])
            ),
        );
        row
    }

    // The line with the name of the work, above its covers
    pub fn line(&self) -> &gtk::Box {
        &self.imp().line
    }

    // Shows a work: its name, then, dimmed, what it arranges and the
    // collection it's in, the number of its recordings, and a heart before
    // it for a favorite. A work that changed is shown again in the row bound
    // to it, as the list binds the row again only for another item
    pub fn show(&self, work: &library::WorkInfo) {
        let imp = self.imp();
        // What an arrangement is, e.g. "for two pianos, Busoni", tells it
        // from the work it arranges, whose title it often has
        let arrangement = work.arrangement.as_ref().map(Arrangement::shown);
        let more = [arrangement.as_deref(), work.collection.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        imp.name.set_markup(&with_dimmed(&work.name, &more));
        imp.count.set_label(&work.recordings.to_string());
        imp.heart.set_visible(work.favorite);
    }

    // The covers of the albums of the work, once it has opened
    pub fn covers(&self) -> Option<RevealBin> {
        self.imp().covers.borrow().clone()
    }

    // The covers, made by `make` and put under the line the first time
    pub fn covers_or_insert(&self, make: impl FnOnce() -> RevealBin) -> RevealBin {
        if let Some(bin) = self.covers() {
            return bin;
        }
        let bin = make();
        self.append(&bin);
        self.imp().covers.replace(Some(bin.clone()));
        bin
    }

    // A cover among the covers: the flow box's child that holds its cell,
    // which takes the focus. It's the one of the recording of the track on
    // the album, or else the album's first, see `CoverRef`
    pub fn cover_of(&self, cover: &CoverRef) -> Option<gtk::Widget> {
        let flowbox = self.covers()?.child().and_downcast::<gtk::FlowBox>()?;
        let of_album: Vec<(gtk::Widget, AlbumCell)> =
            std::iter::successors(flowbox.first_child(), WidgetExt::next_sibling)
                .filter_map(|child| {
                    let cell = child.first_child().and_downcast::<AlbumCell>()?;
                    let album = cell.album()?;
                    (album.id_str() == cover.album).then_some((child, cell))
                })
                .collect();
        let holds_track = |cell: &AlbumCell| {
            cover
                .track
                .as_ref()
                .is_some_and(|track| cell.work().is_some_and(|work| work.paths.contains(track)))
        };
        of_album
            .iter()
            .find(|(_, cell)| holds_track(cell))
            .or(of_album.first())
            .map(|(child, _)| child.clone())
    }

    // Asks for the menu of the work from the keyboard, in the middle of the
    // line, as a long press would
    pub fn request_menu(&self) {
        let line = self.line();
        let x = f64::from(line.width()) / 2.0;
        let y = f64::from(line.height()) / 2.0;
        self.emit_by_name::<()>("menu-requested", &[&x, &y]);
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
}
