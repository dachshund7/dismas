// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib};
use log::debug;
use std::sync::LazyLock;

use crate::{
    album_cell::AlbumCell,
    audio::normalize_search_text,
    i18n::{i18n, i18n_f, i18n_k, ni18n_f},
    library::{self, Album, AlbumSort, ScanEvent, ScanResult},
    square_bin::SquareBin,
    utils,
};

// The covers in the grid grow with the width of the columns, between
// these sizes
const MIN_COVER_SIZE: i32 = 160;
const MAX_COVER_SIZE: i32 = 240;

// The space around each cover in the grid, from the margin of the cells
// and the padding of their content in style.css, and the padding of the
// grid on each side
const COVER_SPACING: i32 = 2 * (8 + 8);
const GRID_PADDING: i32 = 12;

// The number of covers kept in memory: the ones on screen, the ones read
// ahead a screenful above and below them, and the last ones scrolled past;
// keeping all of them would take hundreds of megabytes
const MAX_CACHED_COVERS: usize = 150;

// The cells waiting for each cover to load, with the ID of the album each
// showed then, keyed by the path of the cover
type PendingCovers = HashMap<String, Vec<(String, glib::WeakRef<AlbumCell>)>>;

// The size of the cover on the copy of an album that follows the pointer
// while it's dragged
const DRAG_COVER_SIZE: i32 = 64;

// The albums dragged from the grid, with the words of the search each was
// selected in, like the ones the Add button adds, and whether they are
// the selected ones; only Dismas takes them
#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "DismasAlbumDrag")]
pub struct AlbumDrag {
    pub albums: Vec<(String, Vec<String>)>,
    pub selection: bool,
}

// The covers loaded most recently
#[derive(Debug, Default)]
pub struct TextureCache {
    textures: HashMap<String, gdk::Texture>,
    order: VecDeque<String>,
}

impl TextureCache {
    fn get(&mut self, path: &str) -> Option<gdk::Texture> {
        let texture = self.textures.get(path)?.clone();
        // Move it to the back of the queue, as it's in use again
        if let Some(pos) = self.order.iter().position(|p| p == path) {
            let path = self.order.remove(pos).unwrap();
            self.order.push_back(path);
        }
        Some(texture)
    }

    fn insert(&mut self, path: String, texture: gdk::Texture) {
        if self.textures.insert(path.clone(), texture).is_none() {
            self.order.push_back(path);
        }
        while self.order.len() > MAX_CACHED_COVERS {
            if let Some(oldest) = self.order.pop_front() {
                self.textures.remove(&oldest);
            }
        }
    }

    // Whether the cover is in the cache, without counting it as used
    fn contains(&self, path: &str) -> bool {
        self.textures.contains_key(path)
    }

    fn clear(&mut self) {
        self.textures.clear();
        self.order.clear();
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/dachshund7/Dismas/library-view.ui")]
    #[properties(wrapper_type = super::LibraryView)]
    pub struct LibraryView {
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub sort_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub whole_albums_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub whole_albums_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub albums_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub scan_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub update_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub grid_clamp: TemplateChild<adw::ClampScrollable>,
        #[template_child]
        pub select_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub selected_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub play_albums_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub add_albums_button: TemplateChild<gtk::Button>,

        // Whether clicking an album selects it, instead of showing it
        #[property(get, set = Self::set_selection_mode)]
        pub selection_mode: Cell<bool>,
        // The ids of the selected albums; a new search clears them, so
        // they're all among the albums it shows
        pub selected: RefCell<HashSet<String>>,
        // The last album selected or unselected with a click, where
        // Shift+click starts selecting from
        pub selection_anchor: glib::WeakRef<Album>,
        // Tracks are being added to the queue
        pub loading: Cell<bool>,
        // The drag source of the album being dragged, to cancel the drag
        pub drag_source: RefCell<Option<gtk::DragSource>>,

        pub model: RefCell<Option<gio::ListStore>>,
        pub filter: RefCell<Option<gtk::CustomFilter>>,
        pub sorter: RefCell<Option<gtk::CustomSorter>>,
        pub settings: OnceCell<gio::Settings>,
        // The albums matching the search, if there is one, with the
        // number of their tracks that match it and the number of all of them
        pub search_matches: RefCell<Option<HashMap<String, (u32, u32)>>>,
        // The words of the search the matches are for
        pub search_words: RefCell<Vec<String>>,
        // The albums the search matches are added, played, dragged and
        // shown with all their tracks, rather than those that match it;
        // the toggle next to the search and the banner of the album's
        // tracks are bound to it
        #[property(get, set = Self::set_whole_albums, explicit_notify)]
        pub whole_albums: Cell<bool>,
        pub textures: RefCell<TextureCache>,
        pub pending_covers: RefCell<super::PendingCovers>,
        // The cells of the grid that are mapped, which are about the ones
        // on screen
        pub mapped_cells: Cell<u32>,
        // The music folders are being read
        #[property(get)]
        pub scanning: Cell<bool>,
        // The folders changed while they were being read, so they're read
        // again once it's done; two scans would write at once
        pub rescan_pending: Cell<bool>,
        // Bumped for each reload of the albums
        pub reload_generation: Cell<u32>,
        // The music folders that aren't there, as of the last reload, whose
        // albums are hidden
        pub hidden_folders: RefCell<Vec<std::path::PathBuf>>,
        // The albums the grid shows, as they were read
        pub shown_albums: RefCell<Vec<library::AlbumInfo>>,
        // A scan changed tracks or made covers again since the albums were
        // last shown
        pub library_changed: Cell<bool>,
        // What the albums are sorted by, read from the settings once for
        // all the comparisons of a sort
        pub sort_order: Cell<Option<(AlbumSort, bool)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LibraryView {
        const NAME: &'static str = "DismasLibraryView";
        type Type = super::LibraryView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            crate::fit_bin::FitBin::static_type();
            crate::page_switcher::PageSwitcher::static_type();
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("libraryview");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl LibraryView {
        fn set_selection_mode(&self, selection_mode: bool) {
            if self.selection_mode.replace(selection_mode) == selection_mode {
                return;
            }
            if !selection_mode {
                self.obj().unselect_all();
                self.selection_anchor.set(None);
            }
            self.obj().notify_selection_mode();
        }

        // The covers no longer say how many of their tracks the search
        // matches while the whole albums are taken
        fn set_whole_albums(&self, whole_albums: bool) {
            if self.whole_albums.replace(whole_albums) == whole_albums {
                return;
            }
            self.obj().update_match_counts();
            self.obj().notify_whole_albums();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for LibraryView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            crate::utils::setup_primary_menu_button(&self.menu_button);

            let obj = self.obj();
            obj.setup_grid();
            obj.setup_actions();
            obj.connect_signals();
            // It names the selected playlist
            crate::utils::wrap_tooltip(&*self.add_albums_button);
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("album-activated")
                        .param_types([Album::static_type()])
                        .build(),
                    // Add or play the tracks of the selected albums
                    Signal::builder("add-albums").build(),
                    Signal::builder("play-albums").build(),
                    // Albums started or stopped being dragged
                    Signal::builder("albums-drag-begin").build(),
                    Signal::builder("albums-drag-end").build(),
                    // The URIs of the tracks whose files changed since the
                    // last scan, the number of tracks that are new, changed
                    // or gone, and whether the user asked for the scan
                    Signal::builder("scan-finished")
                        .param_types([
                            Vec::<String>::static_type(),
                            u32::static_type(),
                            bool::static_type(),
                        ])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for LibraryView {}
}

glib::wrapper! {
    pub struct LibraryView(ObjectSubclass<imp::LibraryView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for LibraryView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl LibraryView {
    pub fn connect_album_activated<F: Fn(&Self, &Album) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "album-activated",
            false,
            glib::closure_local!(move |view: &Self, album: &Album| f(view, album)),
        )
    }

    pub fn connect_add_albums<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "add-albums",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_play_albums<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "play-albums",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_albums_drag_begin<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "albums-drag-begin",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_albums_drag_end<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "albums-drag-end",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_scan_finished<F: Fn(&Self, Vec<String>, u32, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "scan-finished",
            false,
            glib::closure_local!(move |view: &Self,
                                       uris: Vec<String>,
                                       n_changed: u32,
                                       requested: bool| {
                f(view, uris, n_changed, requested);
            }),
        )
    }

    fn setup_grid(&self) {
        let imp = self.imp();

        let model = gio::ListStore::new::<Album>();

        let filter = gtk::CustomFilter::new(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            true,
            move |item| {
                let album = item.downcast_ref::<Album>().unwrap();
                match this.imp().search_matches.borrow().as_ref() {
                    Some(matches) => matches.contains_key(&album.id()),
                    None => true,
                }
            }
        ));

        let sorter = gtk::CustomSorter::new(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            gtk::Ordering::Equal,
            move |a, b| {
                let a = a.downcast_ref::<Album>().unwrap();
                let b = b.downcast_ref::<Album>().unwrap();
                let (sort, descending) = this.cached_sort_order();
                a.compare(b, sort, descending).into()
            }
        ));

        let sort_model = gtk::SortListModel::new(Some(model.clone()), Some(sorter.clone()));
        let filter_model = gtk::FilterListModel::new(Some(sort_model), Some(filter.clone()));
        let selection = gtk::NoSelection::new(Some(filter_model));

        imp.grid_view.set_model(Some(&selection));
        imp.grid_view.set_factory(Some(&self.album_factory()));

        // Past the width of the most columns with the largest covers, the
        // grid stays centered, rather than spreading the covers apart
        let grid_width = imp.grid_view.max_columns() as i32 * (MAX_COVER_SIZE + COVER_SPACING)
            + 2 * GRID_PADDING;
        imp.grid_clamp.set_maximum_size(grid_width);
        imp.grid_clamp.set_tightening_threshold(grid_width);

        imp.model.replace(Some(model));
        imp.filter.replace(Some(filter));
        imp.sorter.replace(Some(sorter));
    }

    fn app_settings(&self) -> &gio::Settings {
        self.imp().settings.get_or_init(utils::settings_manager)
    }

    // What the albums are sorted by, and whether in descending order
    fn sort_order(&self) -> (AlbumSort, bool) {
        let settings = self.app_settings();
        (
            AlbumSort::from_nick(&settings.string("album-sort")),
            settings.string("album-sort-order") == "descending",
        )
    }

    // The sort order, as of the last time the settings changed; sorting
    // compares the albums thousands of times
    fn cached_sort_order(&self) -> (AlbumSort, bool) {
        let cached = &self.imp().sort_order;
        cached.get().unwrap_or_else(|| {
            let order = self.sort_order();
            cached.set(Some(order));
            order
        })
    }

    fn setup_actions(&self) {
        let settings = self.app_settings();

        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&settings.create_action("album-sort"));
        actions.add_action(&settings.create_action("album-sort-order"));
        self.insert_action_group("library", Some(&actions));

        // Picking what to sort by also picks the direction that suits it;
        // it can be reversed afterwards
        settings.connect_changed(
            Some("album-sort"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |settings, _| {
                    let (sort, _) = this.sort_order();
                    let order = if sort.default_descending() {
                        "descending"
                    } else {
                        "ascending"
                    };
                    if settings.string("album-sort-order") == order {
                        this.resort();
                    } else {
                        // Resorts the albums when the order changes
                        let _ = settings.set_string("album-sort-order", order);
                    }
                }
            ),
        );
        settings.connect_changed(
            Some("album-sort-order"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| this.resort()
            ),
        );

        self.update_sort_icon();
    }

    fn set_scanning(&self, scanning: bool) {
        if self.imp().scanning.replace(scanning) != scanning {
            self.notify_scanning();
        }
    }

    fn resort(&self) {
        self.imp().sort_order.set(None);
        if let Some(sorter) = self.imp().sorter.borrow().as_ref() {
            sorter.changed(gtk::SorterChange::Different);
        }
        self.update_sort_icon();
    }

    fn update_sort_icon(&self) {
        let (_, descending) = self.sort_order();
        self.imp().sort_button.set_icon_name(if descending {
            "view-sort-descending-symbolic"
        } else {
            "view-sort-ascending-symbolic"
        });
    }

    fn album_factory(&self) -> gtk::ListItemFactory {
        let factory = gtk::SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();

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
                overlay.add_overlay(&match_count_badge(item));
                let cover = SquareBin::new(&overlay, MIN_COVER_SIZE, MAX_COVER_SIZE);
                let content = AlbumCell::new(&cover, &picture);

                // GridView keeps hundreds of cells bound around the visible
                // ones, and binds many of them again at about every frame
                // while the grid scrolls, as its anchor moves back and forth,
                // but it hides the ones off screen. So a cell only shows its
                // cover while it's mapped; the cache keeps the most recent
                // ones for when they come back into view
                picture.connect_map(clone!(
                    #[weak]
                    this,
                    #[weak]
                    content,
                    move |_| {
                        let mapped = &this.imp().mapped_cells;
                        mapped.set(mapped.get() + 1);
                        this.show_cover(&content);
                    }
                ));
                picture.connect_unmap(clone!(
                    #[weak]
                    this,
                    move |picture| {
                        picture.set_paintable(gdk::Paintable::NONE);
                        let mapped = &this.imp().mapped_cells;
                        mapped.set(mapped.get().saturating_sub(1));
                    }
                ));

                // Ctrl+click selects the album, and Shift+click in selection
                // mode the albums from the last one clicked, like in the
                // queue
                let click = gtk::GestureClick::new();
                click.connect_pressed(clone!(
                    #[weak]
                    this,
                    #[weak]
                    item,
                    move |gesture, _, _, _| {
                        let state = gesture.current_event_state();
                        let range =
                            state.contains(gdk::ModifierType::SHIFT_MASK) && this.selection_mode();
                        if !range && !state.contains(gdk::ModifierType::CONTROL_MASK) {
                            return;
                        }
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        if let Some(album) = item.item().and_downcast::<Album>() {
                            if range {
                                this.select_range(&album);
                            } else {
                                this.toggle_album(&album);
                            }
                        }
                    }
                ));
                content.add_controller(click);
                this.setup_album_drag(&content, item, &picture);
                content.set_list_item(item);

                item.set_child(Some(&content));

                item.property_expression("item")
                    .chain_property::<Album>("selected")
                    .bind(&content, "selected", gtk::Widget::NONE);
                this.bind_property("selection-mode", &content, "selection-mode")
                    .sync_create()
                    .build();
            }
        ));

        factory.connect_bind(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(album) = item.item().and_downcast::<Album>() else {
                    return;
                };
                let cell = item.child().and_downcast::<AlbumCell>().unwrap();
                cell.show_album(&album);
                if cell.picture().is_mapped() {
                    this.show_cover(&cell);
                }
            }
        ));

        factory.upcast()
    }

    // An album is dragged to a place in the playlist pane; in selection
    // mode, a selected album takes the other selected ones with it. On a
    // touchscreen, dragging scrolls the grid instead
    fn setup_album_drag(&self, cell: &AlbumCell, item: &gtk::ListItem, picture: &gtk::Picture) {
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gdk::DragAction::COPY);
        drag_source.connect_prepare(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            item,
            #[upgrade_or]
            None,
            move |source, _, _| {
                if this.imp().loading.get()
                    || source
                        .current_event_device()
                        .is_some_and(|device| device.source() == gdk::InputSource::Touchscreen)
                {
                    return None;
                }
                let album = item.item().and_downcast::<Album>()?;
                let selection = this.selection_mode() && album.selected();
                let albums = if selection {
                    this.selected_albums()
                } else {
                    vec![(album.id(), this.track_words())]
                };
                let drag = AlbumDrag { albums, selection };
                Some(gdk::ContentProvider::for_value(&drag.to_value()))
            }
        ));
        drag_source.connect_drag_begin(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cell,
            #[weak]
            picture,
            move |source, drag| {
                let n_albums = drag
                    .content()
                    .value(AlbumDrag::static_type())
                    .ok()
                    .and_then(|value| value.get::<AlbumDrag>().ok())
                    .map_or(1, |drag| drag.albums.len());
                gtk::DragIcon::for_drag(drag).set_child(Some(&album_drag_icon(
                    picture.paintable().as_ref(),
                    n_albums,
                )));
                drag.set_hotspot(DRAG_COVER_SIZE / 2, DRAG_COVER_SIZE / 2);

                // The dragged albums fade in the grid
                cell.add_css_class("dragging");
                if n_albums > 1 {
                    this.imp().grid_view.add_css_class("dragging-selection");
                }
                this.imp().drag_source.replace(Some(source.clone()));
                this.emit_by_name::<()>("albums-drag-begin", &[]);
            }
        ));
        drag_source.connect_drag_end(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cell,
            move |_, _, _| {
                cell.remove_css_class("dragging");
                this.imp().grid_view.remove_css_class("dragging-selection");
                this.imp().drag_source.take();
                this.emit_by_name::<()>("albums-drag-end", &[]);
            }
        ));
        cell.add_controller(drag_source);
    }

    // Cancels dragging albums, and says if they were being dragged
    pub fn cancel_drag(&self) -> bool {
        let source = self.imp().drag_source.borrow().clone();
        source.is_some_and(|source| utils::cancel_drag(&source))
    }

    // Shows the cover of the album in the cell, which has just come on
    // screen, once it has been read in a separate thread, and reads ahead
    // the covers around it. Nothing waits for the main loop to be idle: it
    // never is while the grid scrolls, as GTK binds its cells again at about
    // every frame
    fn show_cover(&self, cell: &AlbumCell) {
        let Some(album) = cell.album() else {
            return;
        };
        if let Some(path) = album.cover_path() {
            let texture = self.imp().textures.borrow_mut().get(&path);
            match texture {
                Some(texture) => cell.picture().set_paintable(Some(&texture)),
                None => self.read_cover(path, Some((album.id(), cell))),
            }
        }
        if let Some(position) = cell.position() {
            self.read_ahead(position);
        }
    }

    // Reads ahead the covers of the albums a screenful before and after the
    // one at this position, which has just come on screen, into the cache:
    // GridView only shows a cell once it's within a few pixels of the
    // screen, which would leave the albums coming into view without their
    // covers while they're read. Those on screen and a screenful on each
    // side have to fit in the cache, or the covers read ahead would push
    // out the ones on screen: while the grid opens, GTK maps a few hundred
    // cells before it hides those off screen, and a large screen can show
    // as many
    fn read_ahead(&self, position: u32) {
        let imp = self.imp();
        let Some(model) = imp.grid_view.model() else {
            return;
        };
        let screenful = imp
            .mapped_cells
            .get()
            .clamp(1, (MAX_CACHED_COVERS / 3) as u32);
        let end = position
            .saturating_add(screenful)
            .min(model.n_items().saturating_sub(1));
        for position in position.saturating_sub(screenful)..=end {
            if let Some(path) = model
                .item(position)
                .and_downcast::<Album>()
                .and_then(|album| album.cover_path())
                && !imp.textures.borrow().contains(&path)
            {
                self.read_cover(path, None);
            }
        }
    }

    // Reads the cover in a separate thread, into the cache, and for the
    // cell showing the album with this ID, if there's one; the cell can
    // show another album by the time it's read
    fn read_cover(&self, path: String, cell: Option<(String, &AlbumCell)>) {
        let imp = self.imp();
        if let Some(texture) = imp.textures.borrow_mut().get(&path) {
            if let Some((_, cell)) = cell {
                cell.picture().set_paintable(Some(&texture));
            }
            return;
        }

        // Another cell, or reading ahead, is already waiting for the same
        // cover
        let waiting = cell.map(|(id, cell)| (id, cell.downgrade()));
        let mut pending = imp.pending_covers.borrow_mut();
        if let Some(others) = pending.get_mut(&path) {
            others.extend(waiting);
            return;
        }
        pending.insert(path.clone(), waiting.into_iter().collect());

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let texture = {
                    let path = path.clone();
                    gio::spawn_blocking(move || gdk::Texture::from_filename(path).ok())
                        .await
                        .ok()
                        .flatten()
                };

                let imp = this.imp();
                let waiting = imp.pending_covers.borrow_mut().remove(&path);
                let Some(texture) = texture else {
                    return;
                };

                for (id, cell) in waiting.into_iter().flatten() {
                    if let Some(cell) = cell.upgrade()
                        && cell.album().is_some_and(|album| album.id() == id)
                        && cell.picture().is_mapped()
                    {
                        cell.picture().set_paintable(Some(&texture));
                    }
                }
                imp.textures.borrow_mut().insert(path, texture);
            }
        ));
    }

    fn connect_signals(&self) {
        let imp = self.imp();

        imp.grid_view.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            move |grid, pos| {
                let album = grid
                    .model()
                    .and_then(|m| m.item(pos))
                    .and_downcast::<Album>();
                if let Some(album) = album {
                    if this.selection_mode() {
                        this.toggle_album(&album);
                    } else {
                        this.emit_by_name::<()>("album-activated", &[&album]);
                    }
                }
            }
        ));

        imp.search_entry.connect_search_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                // The selection was made among the albums of the previous
                // search, so it would be confusing to keep it
                this.set_selection_mode(false);
                this.update_search();
            }
        ));

        // Esc clears the search, which also leaves selection mode
        imp.search_entry.connect_stop_search(clone!(
            #[weak(rename_to = this)]
            self,
            move |entry| {
                this.set_selection_mode(false);
                entry.set_text("");
            }
        ));
        crate::utils::focus_on_clear(&imp.search_entry);

        imp.select_all_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.select_all()
        ));

        imp.add_albums_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("add-albums", &[])
        ));

        imp.play_albums_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.emit_by_name::<()>("play-albums", &[])
        ));

        self.update_selection();
    }

    // Looks for the albums with a track that contains every word of the
    // search, in any of its tags, like searching the queue
    fn update_search(&self) {
        let text = self.imp().search_entry.text();
        let words: Vec<String> = normalize_search_text(&text)
            .split_whitespace()
            .map(ToString::to_string)
            .collect();

        if words.is_empty() {
            self.set_search_matches(Vec::new(), None);
            return;
        }

        let hidden = self.imp().hidden_folders.borrow().clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok((words, matches)) = gio::spawn_blocking(move || {
                    let matches = library::search_albums(&words, &hidden);
                    (words, matches)
                })
                .await
                else {
                    return;
                };

                // Results for an older search arriving late
                if this.imp().search_entry.text() != text {
                    return;
                }
                this.set_search_matches(words, Some(matches));
            }
        ));
    }

    fn set_search_matches(&self, words: Vec<String>, matches: Option<HashMap<String, (u32, u32)>>) {
        let imp = self.imp();
        // A new search starts again from the tracks that match it, as the
        // albums it finds might not need whole albums. That's once its
        // matches are in, as Whole Albums can still be pressed for the
        // search before while they're read
        let new_search = *imp.search_words.borrow() != words;
        imp.search_words.replace(words);
        imp.search_matches.replace(matches);
        if new_search {
            self.set_whole_albums(false);
        }
        self.update_match_counts();
        self.update_whole_albums_button();
        if let Some(filter) = self.imp().filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
    }

    // The covers of the albums the search matches only partly say how many
    // of their tracks it does, unless the whole albums are taken
    fn update_match_counts(&self) {
        let imp = self.imp();
        let matches = imp.search_matches.borrow();
        let whole_albums = imp.whole_albums.get();
        let Some(model) = imp.model.borrow().clone() else {
            return;
        };
        for album in model.iter::<Album>().flatten() {
            let (matching, total) = matches
                .as_ref()
                .filter(|_| !whole_albums)
                .and_then(|matches| matches.get(&album.id()).copied())
                .filter(|(matching, total)| matching < total)
                .unwrap_or_default();
            if album.matching_tracks() != matching {
                album.set_matching_tracks(matching);
            }
            if album.track_count() != total {
                album.set_track_count(total);
            }
        }
    }

    // Whole Albums is only offered while the search matches only some of
    // the tracks of albums
    fn update_whole_albums_button(&self) {
        let imp = self.imp();
        let partial = imp
            .search_matches
            .borrow()
            .as_ref()
            .is_some_and(|matches| matches.values().any(|(matching, total)| matching < total));
        imp.whole_albums_revealer.set_reveal_child(partial);
    }

    // The words of the search, which the tracks of an album it found
    // match
    pub fn search_words(&self) -> Vec<String> {
        self.imp().search_words.borrow().clone()
    }

    // The words that the tracks taken from an album have to match, to be
    // added, played, dragged or shown: those of the search, or none when
    // the whole albums are taken
    pub fn track_words(&self) -> Vec<String> {
        let imp = self.imp();
        if imp.whole_albums.get() {
            Vec::new()
        } else {
            imp.search_words.borrow().clone()
        }
    }

    // The album with an id, if it's in the grid
    pub fn album(&self, album_id: &str) -> Option<Album> {
        self.imp()
            .model
            .borrow()
            .as_ref()?
            .iter::<Album>()
            .flatten()
            .find(|a| a.id() == album_id)
    }

    // Scrolls the grid to an album, clearing the search if it hides it
    pub fn reveal_album(&self, album_id: &str) -> Option<Album> {
        let imp = self.imp();
        let album = self.album(album_id)?;

        let hidden = imp
            .search_matches
            .borrow()
            .as_ref()
            .is_some_and(|m| !m.contains_key(album_id));
        if hidden {
            imp.search_entry.set_text("");
            self.set_search_matches(Vec::new(), None);
        }

        let model = imp.grid_view.model()?;
        let pos = (0..model.n_items())
            .find(|&i| model.item(i).and_downcast::<Album>().as_ref() == Some(&album))?;
        imp.grid_view
            .scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);

        Some(album)
    }

    pub fn focus_search(&self) {
        self.imp().search_entry.grab_focus();
    }

    pub fn search_entry(&self) -> gtk::SearchEntry {
        self.imp().search_entry.get()
    }

    fn set_album_selected(&self, album: &Album, selected: bool) {
        album.set_selected(selected);
        let mut ids = self.imp().selected.borrow_mut();
        if selected {
            ids.insert(album.id());
        } else {
            ids.remove(&album.id());
        }
        drop(ids);
        self.update_selection();
    }

    // Selects or unselects an album, entering selection mode if needed
    fn toggle_album(&self, album: &Album) {
        if self.selection_mode() {
            self.set_album_selected(album, !album.selected());
        } else {
            self.set_selection_mode(true);
            self.set_album_selected(album, true);
        }
        self.imp().selection_anchor.set(Some(album));
    }

    // Selects the albums from the last one clicked to this one, in the
    // order of the grid; without a last one, it toggles the album like a
    // click
    fn select_range(&self, album: &Album) {
        let anchor = self.imp().selection_anchor.upgrade();
        let Some(model) = self.imp().grid_view.model() else {
            return;
        };
        // The items of the selection model are typed as GObject, so
        // `iter::<Album>()` would panic
        let albums: Vec<Album> = (0..model.n_items())
            .filter_map(|i| model.item(i).and_downcast::<Album>())
            .collect();
        let position = |album: &Album| albums.iter().position(|a| a == album);

        let Some((from, to)) = anchor.as_ref().and_then(position).zip(position(album)) else {
            self.toggle_album(album);
            return;
        };
        for album in &albums[from.min(to)..=from.max(to)] {
            if !album.selected() {
                self.set_album_selected(album, true);
            }
        }
    }

    // Selects all the albums shown by the search
    pub fn select_all(&self) {
        self.set_selection_mode(true);
        let Some(model) = self.imp().grid_view.model() else {
            return;
        };
        // The items of the selection model are typed as GObject, so
        // `iter::<Album>()` would panic
        let albums: Vec<Album> = (0..model.n_items())
            .filter_map(|i| model.item(i).and_downcast::<Album>())
            .collect();
        for album in albums {
            if !album.selected() {
                self.set_album_selected(&album, true);
            }
        }
    }

    fn unselect_all(&self) {
        if let Some(model) = self.imp().model.borrow().as_ref() {
            for album in model.iter::<Album>().flatten() {
                album.set_selected(false);
            }
        }
        self.imp().selected.borrow_mut().clear();
        self.update_selection();
    }

    // The selected albums, in the order of the grid, with the words their
    // tracks have to match to be taken
    pub fn selected_albums(&self) -> Vec<(String, Vec<String>)> {
        let imp = self.imp();
        let selected = imp.selected.borrow();
        let words = self.track_words();
        let Some(model) = imp.grid_view.model() else {
            return Vec::new();
        };
        // The items of the selection model are typed as GObject, so
        // `iter::<Album>()` would panic
        (0..model.n_items())
            .filter_map(|i| model.item(i).and_downcast::<Album>())
            .map(|album| album.id())
            .filter(|id| selected.contains(id))
            .map(|id| (id, words.clone()))
            .collect()
    }

    // The label of the button that adds the albums to the selected playlist
    pub fn set_add_label(&self, label: &str) {
        crate::utils::set_playlist_button_label(&self.imp().add_albums_button, label);
    }

    // Albums can't be added while tracks are being added to the queue
    pub fn set_adding_enabled(&self, enabled: bool) {
        self.imp().loading.set(!enabled);
        self.update_selection();
    }

    fn update_selection(&self) {
        let imp = self.imp();
        let n_selected = imp.selected.borrow().len() as u32;
        let label = if n_selected == 0 {
            i18n("No album selected")
        } else {
            ni18n_f(
                // Translators: The '{}' must be left unmodified, and
                // it is expanded to the number of albums selected
                "{} album selected",
                "{} albums selected",
                n_selected,
                &[&n_selected.to_string()],
            )
        };
        imp.selected_label.set_label(&label);

        let can_add = n_selected > 0 && !imp.loading.get();
        imp.play_albums_button.set_sensitive(can_add);
        imp.add_albums_button.set_sensitive(can_add);
    }

    // Reads the albums from the database, in a separate thread; the albums
    // of the last reload are shown, e.g. the one after a scan rather than
    // the one before it, if it takes longer. Those of the music folders
    // that aren't there are hidden, as they can't play, and so are those of
    // the folders left out, until the scan forgets them
    pub fn reload(&self) {
        let imp = self.imp();
        let generation = imp.reload_generation.get().wrapping_add(1);
        imp.reload_generation.set(generation);
        let folders = library::music_folders(self.app_settings());

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let (hidden, albums) = gio::spawn_blocking(move || {
                    let roots = library::roots(&folders);
                    let hidden = [roots.unavailable, roots.excluded].concat();
                    let albums = library::albums(&hidden);
                    (hidden, albums)
                })
                .await
                .unwrap_or_default();
                if this.imp().reload_generation.get() == generation {
                    this.imp().hidden_folders.replace(hidden);
                    this.show_albums(albums);
                }
            }
        ));
    }

    fn show_albums(&self, albums: Vec<library::AlbumInfo>) {
        let imp = self.imp();

        // After a scan that changed nothing, as at most starts, the albums
        // read again are the same, and the grid stays as it is, rather
        // than read the covers it shows again
        let changed = imp.library_changed.take();
        if !changed && *imp.shown_albums.borrow() == albums {
            return;
        }
        imp.shown_albums.replace(albums.clone());

        let albums: Vec<Album> = albums.into_iter().map(Album::new).collect();
        debug!("Library: {} albums", albums.len());

        // Keep the selection, minus the albums that are gone
        {
            let mut selected = imp.selected.borrow_mut();
            let ids: HashSet<String> = albums.iter().map(Album::id).collect();
            selected.retain(|id| ids.contains(id));
            for album in &albums {
                album.set_selected(selected.contains(&album.id()));
            }
        }
        self.update_selection();

        if let Some(model) = imp.model.borrow().as_ref() {
            model.splice(0, model.n_items(), &albums);
        }

        // A cover made again can keep its path; otherwise, the covers in
        // the cache are still the ones of these albums
        if changed {
            imp.textures.borrow_mut().clear();
        }
        self.update_stack();

        // The tracks might have changed since the last search
        if imp.search_matches.borrow().is_some() {
            self.update_search();
        }
    }

    fn update_stack(&self) {
        let imp = self.imp();
        let n_albums = imp.model.borrow().as_ref().map_or(0, ListModelExt::n_items);
        let page = if n_albums > 0 {
            "albums"
        } else if imp.scanning.get() {
            "scanning"
        } else {
            "empty"
        };
        imp.albums_stack.set_visible_child_name(page);

        // When the grid is already showing, a thin bar at the bottom is
        // enough
        imp.update_progress
            .set_visible(page == "albums" && imp.scanning.get());
    }

    // Shows the albums we know about, then looks for changes in the music
    // folders in a separate thread; `requested` is whether the user asked
    // for it, to tell them what it found. During a scan, another one follows
    // it, as the folders might have changed since it started; the user can't
    // ask for one then, as the action is off
    pub fn start_scan(&self, requested: bool) {
        let imp = self.imp();
        if imp.scanning.get() {
            imp.rescan_pending.set(true);
            return;
        }
        self.set_scanning(true);
        self.run_scan(requested);
    }

    fn run_scan(&self, requested: bool) {
        let imp = self.imp();
        let folders = library::music_folders(self.app_settings());

        self.reload();
        imp.scan_progress.set_fraction(0.0);
        imp.scan_progress.set_text(None);

        // Looking for the files that changed takes a while in a large
        // folder, before there is anything to count
        let counting = Rc::new(Cell::new(false));
        glib::timeout_add_local(
            std::time::Duration::from_millis(100),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[strong]
                counting,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let imp = this.imp();
                    if counting.get() || !imp.scanning.get() {
                        return glib::ControlFlow::Break;
                    }
                    imp.scan_progress.pulse();
                    imp.update_progress.pulse();
                    glib::ControlFlow::Continue
                }
            ),
        );

        let (sender, receiver) = async_channel::unbounded();
        let thumbnail_size = thumbnail_size();
        std::thread::spawn(move || library::scan(&folders, thumbnail_size, &sender));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let mut result = ScanResult::default();
                while let Ok(event) = receiver.recv().await {
                    let imp = this.imp();
                    match event {
                        ScanEvent::Progress(done, total) => {
                            counting.set(true);
                            let fraction = done as f64 / total.max(1) as f64;
                            let text = i18n_k(
                                // Translators: the `{done}` and `{total}` must be left
                                // unmodified; they are the number of tracks read so far
                                // and the number of tracks to read
                                "{done} of {total} tracks",
                                &[("done", &done.to_string()), ("total", &total.to_string())],
                            );
                            imp.scan_progress.set_fraction(fraction);
                            imp.scan_progress.set_text(Some(&text));
                            imp.update_progress.set_fraction(fraction);
                        }
                        ScanEvent::Finished(finished) => {
                            result = finished;
                            break;
                        }
                    }
                }

                // The progress bars stop pulsing for this scan, even when
                // another one follows. The channel also closes if the scan
                // panicked
                counting.set(true);
                if result.n_changed > 0 || result.covers_changed {
                    this.imp().library_changed.set(true);
                }
                let uris: Vec<String> = result
                    .updated
                    .iter()
                    .map(|path| gio::File::for_path(path).uri().to_string())
                    .collect();
                let n_changed = u32::try_from(result.n_changed).unwrap_or(u32::MAX);

                // The next scan goes on without stopping the spinner
                if this.imp().rescan_pending.take() {
                    this.emit_by_name::<()>("scan-finished", &[&uris, &n_changed, &requested]);
                    this.run_scan(false);
                    return;
                }
                this.set_scanning(false);
                this.reload();
                this.emit_by_name::<()>("scan-finished", &[&uris, &n_changed, &requested]);
            }
        ));
    }
}

// The width the covers of the grid are made at: the largest the grid shows
// them, in the pixels of the monitor with the largest scale. A cover twice
// as large as it's shown takes four times the memory, and the time to read
fn thumbnail_size() -> u32 {
    let scale = gdk::Display::default()
        .and_then(|display| {
            display
                .monitors()
                .iter::<gdk::Monitor>()
                .flatten()
                .map(|monitor| monitor.scale_factor())
                .max()
        })
        .unwrap_or(1)
        .clamp(1, 2);
    (MAX_COVER_SIZE * scale) as u32
}

// The copy of an album that follows the pointer while it's dragged: its
// cover, with the number of albums when there are several
// The badge on the cover of an album the search matches only partly,
// with the number of its tracks that match and the number of all of them,
// e.g. "9 of 12"
fn match_count_badge(item: &gtk::ListItem) -> gtk::Label {
    let badge = gtk::Label::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(6)
        .margin_bottom(6)
        .can_target(false)
        .css_classes(["match-count"])
        .build();

    let album = item.property_expression("item");
    let matching = album.chain_property::<Album>("matching-tracks");
    let total = album.chain_property::<Album>("track-count");
    gtk::ClosureExpression::new::<String>(
        [&matching, &total],
        glib::closure!(|_: Option<glib::Object>, matching: u32, total: u32| {
            i18n_f(
                // Translators: The first '{}' is the number of tracks of an
                // album that match the search, and the second the number of
                // all of them; both must be left unmodified
                "{} of {}",
                &[&matching.to_string(), &total.to_string()],
            )
        }),
    )
    .bind(&badge, "label", gtk::Widget::NONE);
    matching
        .chain_closure::<bool>(glib::closure!(
            |_: Option<glib::Object>, matching: u32| matching > 0
        ))
        .bind(&badge, "visible", gtk::Widget::NONE);

    badge
}

fn album_drag_icon(cover: Option<&gdk::Paintable>, n_albums: usize) -> gtk::Widget {
    let icon = gtk::Box::builder()
        .spacing(8)
        .css_classes(["album-drag-icon"])
        .build();
    let frame = gtk::Overlay::builder()
        .width_request(DRAG_COVER_SIZE)
        .height_request(DRAG_COVER_SIZE)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["album-cover"])
        .child(
            &gtk::Image::builder()
                .icon_name("folder-music-symbolic")
                .pixel_size(32)
                .css_classes(["dim-label"])
                .build(),
        )
        .build();
    if let Some(cover) = cover {
        let picture = gtk::Picture::builder()
            .paintable(cover)
            .content_fit(gtk::ContentFit::Cover)
            .can_shrink(true)
            .build();
        frame.add_overlay(&picture);
    }
    icon.append(&frame);

    if n_albums > 1 {
        let n_albums = n_albums as u32;
        icon.append(
            &gtk::Label::builder()
                .label(ni18n_f(
                    // Translators: The '{}' must be left unmodified, and
                    // it is expanded to the number of albums being dragged
                    "{} album",
                    "{} albums",
                    n_albums,
                    &[&n_albums.to_string()],
                ))
                .valign(gtk::Align::Center)
                .css_classes(["drag-count"])
                .build(),
        );
    }
    icon.upcast()
}
