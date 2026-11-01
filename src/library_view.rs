// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

mod work_row;
mod works;

use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib};
use log::debug;
use std::sync::LazyLock;

use crate::{
    album_cell::AlbumCell,
    audio::{Arrangement, PerformerKind, normalize_search_text},
    cover_image,
    i18n::{i18n, i18n_k, ni18n_f},
    library::{
        self, Album, AlbumSort, Counted, Favorites, ScanEvent, ScanResult, ThumbnailUpdate,
        TrackFilter,
    },
    play_stats, utils,
    work_sections::{WorkSectionModel, same_section_of},
    zoom::{self, Zoom},
    zoom_row::ZoomRow,
};

// The covers in the grid grow with the width of the columns, between
// these sizes, at each step of the zoom from the smallest (the
// `cover-zoom` setting, from MIN_COVER_ZOOM, with 0 for the normal size,
// 160 to 240). GridView picks how many columns it has by the covers'
// smallest size, and stretches them to fill the row, so each step starts
// where the one before ends: when they overlapped, a step could leave the
// same columns, as wide as before
const COVER_SIZES: [(i32, i32); 4] = [(90, 120), (120, 160), (160, 240), (240, 320)];
const MIN_COVER_ZOOM: i32 = -2;
const MAX_COVER_ZOOM: i32 = 1;

// The widest the grid gets, at any step: six columns of the largest covers
// at the normal size. Past it, it stays centered, rather than spreading
// the covers apart. The grid keeps 30 rows of cells bound, so each column
// makes scrolling slower, even when fewer of them fit
const MAX_GRID_WIDTH: i32 = 6 * (240 + COVER_SPACING);

// The space around each cover in the grid, from the margin of the cells
// and the padding of their content in style.css, and the padding of the
// grid on each side
const COVER_SPACING: i32 = 2 * (8 + 8);
const GRID_PADDING: i32 = 12;

// The number of covers kept in memory at the largest size the grid shows
// them: the ones on screen, the ones read ahead a screenful above and below
// them, and the last ones scrolled past; keeping all of them would take
// hundreds of megabytes. Smaller covers take less, so more of them are
// kept, as more fit on screen (`cache_capacity`)
const MAX_CACHED_COVERS: usize = 150;

// The cells waiting for each cover to load, with the ID of the album each
// showed then, keyed by the path of the cover
type PendingCovers = HashMap<String, Vec<(String, glib::WeakRef<AlbumCell>)>>;

// The size of the cover on the copy of an album that follows the pointer
// while it's dragged
const DRAG_COVER_SIZE: i32 = 64;

// How long after a scan the music folders that can't be watched are read
// again when the albums are shown. Reading a network share or a folder
// from the document portal takes a while, and going back and forth
// between the pages would read it each time
const UNWATCHED_RESCAN_INTERVAL: Duration = Duration::from_secs(5 * 60);

// The pages of the window that this view shows: the albums of the music
// folders, which are taken whole, the same albums, of which only the tracks
// the search matches are taken, or their works. They're one widget, which
// keeps the search, the composer and the performer picked from one page to
// the other
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "DismasLibraryPage")]
pub enum LibraryPage {
    #[default]
    Albums,
    Tracks,
    Works,
}

impl LibraryPage {
    // The name of the window's page
    pub fn name(self) -> &'static str {
        match self {
            Self::Albums => "albums",
            Self::Tracks => "tracks",
            Self::Works => "works",
        }
    }

    // The page with the name of a window's page, if it's one of these
    pub fn named(name: &str) -> Option<Self> {
        [Self::Albums, Self::Tracks, Self::Works]
            .into_iter()
            .find(|page| page.name() == name)
    }

    // Whether the page shows the grid of the albums
    pub fn shows_grid(self) -> bool {
        self != Self::Works
    }

    // Whether taking an album from the grid takes only the tracks the
    // search matches, rather than the whole album
    fn takes_matches(self) -> bool {
        self == Self::Tracks
    }
}

// The albums dragged from the grid, with the words of the search each was
// selected in, like the ones the Add button adds, and whether they are
// the selected ones; only Dismas takes them
#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "DismasAlbumDrag")]
pub struct AlbumDrag {
    pub albums: Vec<(String, TrackFilter)>,
    pub selection: bool,
}

// The covers loaded most recently
#[derive(Debug)]
pub struct TextureCache {
    textures: HashMap<String, gdk::Texture>,
    order: VecDeque<String>,
    capacity: usize,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            order: VecDeque::new(),
            capacity: MAX_CACHED_COVERS,
        }
    }
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
        self.trim();
    }

    fn trim(&mut self) {
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.textures.remove(&oldest);
            }
        }
    }

    // The covers it keeps, which take about the same memory at any size
    fn capacity(&self) -> usize {
        self.capacity
    }

    fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        self.trim();
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
        pub select_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub favorites_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub search_problem: TemplateChild<adw::Banner>,
        #[template_child]
        pub composer_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub composer_popover: TemplateChild<gtk::Popover>,
        #[template_child]
        pub composer_search: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub composer_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub performer_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub performer_popover: TemplateChild<gtk::Popover>,
        #[template_child]
        pub performer_search: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub performer_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub page_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub works_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub albums_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub no_favorites_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub empty_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub empty_button: TemplateChild<gtk::Button>,
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
        pub add_albums_button: TemplateChild<adw::SplitButton>,

        // Whether clicking an album selects it, instead of showing it
        #[property(get, set = Self::set_selection_mode)]
        pub selection_mode: Cell<bool>,
        // The page shown; the works are only shown while a track is part
        // of one
        #[property(get, set = Self::set_page, explicit_notify, builder(LibraryPage::default()))]
        pub page: Cell<LibraryPage>,
        #[property(get)]
        pub has_works: Cell<bool>,
        // The step the covers of the grid and of the works are zoomed to,
        // which the cells follow
        #[property(get)]
        pub cover_zoom: Cell<i32>,
        // The works the search finds, and the filter and the generation of
        // the library they were read for, `None` until they're read. The
        // generation goes up each time the library is read again, as the
        // tracks or the hidden folders can have changed
        pub works: RefCell<Option<gio::ListStore>>,
        pub works_filter: RefCell<Option<(TrackFilter, u32)>>,
        pub works_generation: Cell<u32>,
        // The work of the list that's open, showing the covers of its
        // albums, and the rows the list shows its works in, by their keys,
        // to open and close them. The generation goes up each time the
        // covers of a work are read, so that those of one opened before
        // are dropped
        pub expanded_work: RefCell<Option<super::works::ExpandedWork>>,
        pub work_rows: RefCell<HashMap<String, glib::WeakRef<work_row::WorkRow>>>,
        pub covers_generation: Cell<u32>,
        // A work to go to once the works are read, and the number of works
        // gone to, so that a new search's list isn't scrolled to its top
        // once one was
        pub pending_work: RefCell<Option<super::works::PendingWork>>,
        pub works_revealed: Cell<u32>,
        // The ids of the selected albums; a new search clears them, so
        // they're all among the albums it shows
        pub selected: RefCell<HashSet<String>>,
        // The ids of the albums marked as favorites
        pub favorite_albums: RefCell<BTreeSet<String>>,
        // The paths of the favorite tracks, read when the heart first needs
        // them, see `favorite_tracks()`
        pub favorite_tracks: RefCell<Option<Arc<[PathBuf]>>>,
        // Whether the focus of the window is followed, see
        // `setup_lost_focus()`
        pub follows_focus: Cell<bool>,
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
        // The words of the search, and the composer and the performer, that
        // the matches are for
        pub search_filter: RefCell<TrackFilter>,
        // The composer and the performer whose albums the grid shows,
        // picked from the lists next to the search
        pub composer: RefCell<Option<library::ComposerInfo>>,
        pub performer: RefCell<Option<library::PerformerInfo>>,
        pub textures: RefCell<TextureCache>,
        pub pending_covers: RefCell<super::PendingCovers>,
        // Bumped when the size the covers are read at changes, so that the
        // covers still being read at the size before aren't kept
        pub cover_generation: Cell<u32>,
        // The size the last scan made the covers at, 0 before the first
        pub thumbnail_size: Cell<u32>,
        // The buttons that zoom the covers, in the main menu
        pub zoom_row: ZoomRow,
        // The `library` actions, of which zooming is only on short of the
        // ends it goes to
        pub actions: gio::SimpleActionGroup,
        // The cells of the grid that are mapped, which are about the ones
        // on screen
        pub mapped_cells: Cell<u32>,
        // The music folders are being read
        #[property(get)]
        pub scanning: Cell<bool>,
        // The folders changed while they were being read, so they're read
        // again once it's done; two scans would write at once
        pub rescan_pending: Cell<bool>,
        // Watches the folders of the library, to read them again when
        // their files change
        pub watch: OnceCell<Rc<library::FolderWatch>>,
        // A music folder that's there can't be watched, so it's read again
        // when the albums are shown, if the last scan finished a while ago
        pub unwatched: Cell<bool>,
        pub last_scan_finished: Cell<Option<Instant>>,
        // Bumped for each reload of the albums
        pub reload_generation: Cell<u32>,
        // The music folders that aren't there, as of the last reload, whose
        // albums are hidden
        pub hidden_folders: RefCell<Vec<PathBuf>>,
        // No album is shown, and the music folders that are missing or
        // empty have albums, as when their drive isn't plugged in
        pub albums_unavailable: Cell<bool>,
        // The albums the grid shows, as they were read
        pub shown_albums: RefCell<Vec<library::AlbumInfo>>,
        // A scan made covers again since the albums were last shown, which
        // can keep their paths, and whether it changed tracks, which a
        // search can find or not find now
        pub covers_changed: Cell<bool>,
        pub tracks_changed: Cell<bool>,
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

        fn set_page(&self, page: LibraryPage) {
            let before = self.page.replace(page);
            if before == page {
                return;
            }
            // A work to go to is for the page it was asked for on
            self.pending_work.take();
            // The Albums and Tracks pages share the grid and its selection,
            // and the Works page selects among the covers of a work
            if before.shows_grid() != page.shows_grid() {
                self.obj().set_selection_mode(false);
            }
            self.obj().update_view();
            // The heart beside the search shows the favorites of each page,
            // so the albums it shows are searched for again
            if self.favorites_button.is_active() {
                self.obj().update_album_matches();
            }
            self.obj().notify_page();
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

            self.zoom_row.set_action_prefix("library");
            // What used to be on the Playlists page, as it plays what the
            // album pages show; Random Work hides while there's no work,
            // as its action is off then
            let items = gio::Menu::new();
            items.append(
                Some(&crate::i18n::i18n("Play a _Random Album")),
                Some("win.play-random-album"),
            );
            let work = gio::MenuItem::new(
                Some(&crate::i18n::i18n("Play a Random _Work")),
                Some("win.play-random-work"),
            );
            work.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
            items.append_item(&work);
            crate::utils::setup_primary_menu_button(
                &self.menu_button,
                Some(&self.zoom_row),
                Some(items.upcast_ref()),
            );

            let obj = self.obj();
            obj.setup_grid();
            obj.setup_actions();
            obj.setup_zoom();
            obj.setup_watch();
            obj.setup_pickers();
            obj.setup_favorites_filter();
            obj.setup_lost_focus();
            obj.setup_works();
            obj.connect_signals();
            // Moved to a monitor with another scale
            obj.connect_scale_factor_notify(super::LibraryView::reset_covers);
            // It names the selected playlist
            crate::utils::wrap_tooltip(&*self.add_albums_button);
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    // An album was activated; the object holds what its
                    // tracks have to match to be taken, and the names of
                    // what it picks, see `taken_by()`
                    Signal::builder("album-activated")
                        .param_types([Album::static_type(), glib::BoxedAnyObject::static_type()])
                        .build(),
                    // Add or play the tracks of the selected albums
                    Signal::builder("add-albums").build(),
                    Signal::builder("play-albums").build(),
                    // The menu of a work of the list, at a point of its
                    // row; the object holds its `library::WorkInfo`
                    Signal::builder("work-menu-requested")
                        .param_types([
                            glib::BoxedAnyObject::static_type(),
                            gtk::Widget::static_type(),
                            f64::static_type(),
                            f64::static_type(),
                        ])
                        .build(),
                    // The menu of an album, at a point of its cell
                    Signal::builder("album-menu-requested")
                        .param_types([
                            AlbumCell::static_type(),
                            f64::static_type(),
                            f64::static_type(),
                        ])
                        .build(),
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
    pub fn connect_album_activated<F: Fn(&Self, &Album, &TrackFilter, &PickNames) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "album-activated",
            false,
            glib::closure_local!(
                move |view: &Self, album: &Album, taken: &glib::BoxedAnyObject| {
                    let taken = taken.borrow::<(TrackFilter, PickNames)>();
                    f(view, album, &taken.0, &taken.1);
                }
            ),
        )
    }

    pub fn connect_work_menu_requested<
        F: Fn(&Self, &library::WorkInfo, &gtk::Widget, f64, f64) + 'static,
    >(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "work-menu-requested",
            false,
            glib::closure_local!(move |view: &Self,
                                       item: &glib::BoxedAnyObject,
                                       widget: &gtk::Widget,
                                       x: f64,
                                       y: f64| {
                f(view, &item.borrow::<library::WorkInfo>(), widget, x, y);
            }),
        )
    }

    pub fn connect_album_menu_requested<F: Fn(&Self, &AlbumCell, f64, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "album-menu-requested",
            false,
            glib::closure_local!(move |view: &Self, cell: &AlbumCell, x: f64, y: f64| {
                f(view, cell, x, y);
            }),
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
                    Some(matches) => matches.contains_key(album.id_str()),
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
        utils::setup_menu_key(
            &*imp.grid_view,
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cell: &AlbumCell| this.request_album_menu(cell)
            ),
        );

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

        let actions = &self.imp().actions;
        actions.add_action(&settings.create_action("album-sort"));
        actions.add_action(&settings.create_action("album-sort-order"));
        for (name, zoom) in [
            ("zoom-in", Zoom::In),
            ("zoom-out", Zoom::Out),
            ("zoom-reset", Zoom::Reset),
        ] {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    this.zoom_covers(zoom);
                }
            ));
            actions.add_action(&action);
        }
        self.insert_action_group("library", Some(actions));

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
                let content = AlbumCell::new(grid_cover_sizes);
                this.setup_cell(&content, true);
                content.set_list_item(item);

                item.set_child(Some(&content));

                // The covers of the albums the search matches only partly
                // say how many of their tracks it does, on the Tracks page
                let album = item.property_expression("item");
                album.chain_property::<Album>("selected").bind(
                    &content,
                    "selected",
                    gtk::Widget::NONE,
                );
                album.chain_property::<Album>("matching-tracks").bind(
                    &content,
                    "matching",
                    gtk::Widget::NONE,
                );
                album.chain_property::<Album>("track-count").bind(
                    &content,
                    "total",
                    gtk::Widget::NONE,
                );
                // The covers of the favorite albums have a heart
                album.chain_property::<Album>("favorite").bind(
                    &content,
                    "favorite",
                    gtk::Widget::NONE,
                );
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

    // What a cell of an album does, in the grid or elsewhere: it shows its
    // cover while it's on screen, is selected, has a menu, and is dragged
    fn setup_cell(&self, cell: &AlbumCell, grid: bool) {
        self.bind_property("cover-zoom", cell, "zoom")
            .sync_create()
            .build();

        // GridView keeps hundreds of cells bound around the visible ones,
        // and binds many of them again at about every frame while the grid
        // scrolls, as its anchor moves back and forth, but it hides the ones
        // off screen. So a cell only shows its cover while it's mapped; the
        // cache keeps the most recent ones for when they come back into
        // view. The cells of the grid that are mapped tell how many covers
        // a screenful has
        cell.picture().connect_map(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cell,
            move |_| {
                if grid {
                    let mapped = &this.imp().mapped_cells;
                    mapped.set(mapped.get() + 1);
                }
                this.show_cover(&cell);
            }
        ));
        cell.picture().connect_unmap(clone!(
            #[weak(rename_to = this)]
            self,
            move |picture| {
                picture.set_paintable(gdk::Paintable::NONE);
                if grid {
                    let mapped = &this.imp().mapped_cells;
                    mapped.set(mapped.get().saturating_sub(1));
                }
            }
        ));

        // Ctrl+click selects the album, and Shift+click in selection mode
        // the albums from the last one clicked, like in the queue
        let click = gtk::GestureClick::new();
        click.connect_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cell,
            move |gesture, _, _, _| {
                let state = gesture.current_event_state();
                let range = state.contains(gdk::ModifierType::SHIFT_MASK) && this.selection_mode();
                if !range && !state.contains(gdk::ModifierType::CONTROL_MASK) {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(album) = cell.album() {
                    if range {
                        this.select_range(&album);
                    } else {
                        this.toggle_album(&album);
                    }
                }
            }
        ));
        cell.add_controller(click);
        self.bind_property("selection-mode", cell, "selection-mode")
            .sync_create()
            .build();
        self.setup_album_menu(cell);
        self.setup_album_drag(cell);
    }

    // A right click, or a long press on a touchscreen, opens the menu of
    // the album, like those of the tracks
    fn setup_album_menu(&self, cell: &AlbumCell) {
        let request = clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cell,
            move |x: f64, y: f64| {
                if cell.album().is_some() {
                    this.emit_by_name::<()>("album-menu-requested", &[&cell, &x, &y]);
                }
            }
        );
        utils::setup_context_menu(cell, request);
    }

    // The menu of the album of a cell, from the Menu key, in its middle
    fn request_album_menu(&self, cell: &AlbumCell) {
        if cell.album().is_some() {
            let x = f64::from(cell.width()) / 2.0;
            let y = f64::from(cell.height()) / 2.0;
            self.emit_by_name::<()>("album-menu-requested", &[cell, &x, &y]);
        }
    }

    // Shows an album, with what's taken from it, see `taken_by()`, or in
    // selection mode selects or unselects it
    fn activate_album(&self, album: &Album, taken: (TrackFilter, PickNames)) {
        if self.selection_mode() {
            self.toggle_album(album);
            return;
        }
        let taken = glib::BoxedAnyObject::new(taken);
        self.emit_by_name::<()>("album-activated", &[album, &taken]);
    }

    // An album is dragged to a place in the playlist pane; in selection
    // mode, a selected album takes the other selected ones with it. On a
    // touchscreen, dragging scrolls the grid instead
    fn setup_album_drag(&self, cell: &AlbumCell) {
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gdk::DragAction::COPY);
        drag_source.connect_prepare(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cell,
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
                let album = cell.album()?;
                let selection = this.selection_mode() && album.selected();
                let albums = if selection {
                    this.selected_albums()
                } else {
                    vec![(album.id(), this.taken_by(&cell).0)]
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
            move |source, drag| {
                let n_albums = drag
                    .content()
                    .value(AlbumDrag::static_type())
                    .ok()
                    .and_then(|value| value.get::<AlbumDrag>().ok())
                    .map_or(1, |drag| drag.albums.len());
                gtk::DragIcon::for_drag(drag).set_child(Some(&album_drag_icon(
                    cell.picture().paintable().as_ref(),
                    n_albums,
                )));
                drag.set_hotspot(DRAG_COVER_SIZE / 2, DRAG_COVER_SIZE / 2);

                // The dragged albums fade
                cell.add_css_class("dragging");
                if n_albums > 1 {
                    this.add_css_class("dragging-selection");
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
                this.remove_css_class("dragging-selection");
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
        if let Some(path) = album.cover_path_str() {
            let texture = self.imp().textures.borrow_mut().get(path);
            match texture {
                Some(texture) => cell.picture().set_paintable(Some(&texture)),
                None => self.read_cover(path.to_owned(), Some((album.id(), cell))),
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
        let capacity = imp.textures.borrow().capacity();
        let screenful = imp.mapped_cells.get().clamp(1, (capacity / 3) as u32);
        let end = position
            .saturating_add(screenful)
            .min(model.n_items().saturating_sub(1));
        for position in position.saturating_sub(screenful)..=end {
            if let Some(album) = model.item(position).and_downcast::<Album>()
                && let Some(path) = album.cover_path_str()
                && !imp.textures.borrow().contains(path)
            {
                self.read_cover(path.to_owned(), None);
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

        let size = self.cover_pixels();
        let generation = imp.cover_generation.get();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let texture = {
                    let path = path.clone();
                    gio::spawn_blocking(move || cover_image::read_thumbnail(Path::new(&path), size))
                        .await
                        .ok()
                        .flatten()
                };

                let imp = this.imp();
                // Read at a size the covers are no longer shown at; the
                // cells waiting for it read it again
                if imp.cover_generation.get() != generation {
                    return;
                }
                let waiting = imp.pending_covers.borrow_mut().remove(&path);
                let Some(texture) = texture else {
                    return;
                };

                for (id, cell) in waiting.into_iter().flatten() {
                    if let Some(cell) = cell.upgrade()
                        && cell.album().is_some_and(|album| album.id_str() == id)
                        && cell.picture().is_mapped()
                    {
                        cell.picture().set_paintable(Some(&texture));
                    }
                }
                imp.textures.borrow_mut().insert(path, texture);
            }
        ));
    }

    // The width the covers are read at, in the pixels of the screen the
    // grid is on: as large as the grid shows them
    fn cover_pixels(&self) -> u32 {
        let (_, size) = grid_cover_sizes(self.imp().cover_zoom.get());
        (size * self.scale_factor()).max(1) as u32
    }

    // The covers in the cache are at a size they're no longer shown at:
    // they're read again for the cells on screen, which keep the ones they
    // show until then, and the covers still being read are dropped
    fn reset_covers(&self) {
        let imp = self.imp();
        imp.cover_generation
            .set(imp.cover_generation.get().wrapping_add(1));
        imp.textures.borrow_mut().clear();
        imp.pending_covers.borrow_mut().clear();
        for cell in mapped_album_cells(self.upcast_ref()) {
            self.show_cover(&cell);
        }
    }

    // The covers are as large as the `cover-zoom` setting says, with Ctrl
    // and the mouse wheel over them as well as with the keys
    fn setup_zoom(&self) {
        self.app_settings().connect_changed(
            Some("cover-zoom"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| this.apply_cover_zoom()
            ),
        );
        self.apply_cover_zoom();

        // In the capture phase, so that the grid doesn't scroll as well
        let steps = RefCell::new(zoom::ScrollSteps::default());
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        scroll.connect_scroll(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| {
                if !controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                {
                    return glib::Propagation::Proceed;
                }
                let n = steps.borrow_mut().add(dy, controller.unit());
                let zoom = if n > 0 { Zoom::In } else { Zoom::Out };
                for _ in 0..n.unsigned_abs() {
                    this.zoom_covers(zoom);
                }
                glib::Propagation::Stop
            }
        ));
        self.imp().albums_stack.add_controller(scroll);

        zoom::add_pinch(
            &*self.imp().albums_stack,
            clone!(
                #[weak(rename_to = this)]
                self,
                move |zoom| {
                    this.zoom_covers(zoom);
                }
            ),
        );
    }

    // Zooms the covers of the grid and of the works a step, or back to
    // their normal size; false when they're at that end already
    pub fn zoom_covers(&self, zoom: Zoom) -> bool {
        let settings = self.app_settings();
        let step = settings.int("cover-zoom");
        match zoom.step_from(step, MIN_COVER_ZOOM, MAX_COVER_ZOOM) {
            Some(step) => settings.set_int("cover-zoom", step).is_ok(),
            None => false,
        }
    }

    fn apply_cover_zoom(&self) {
        let imp = self.imp();
        let step = self
            .app_settings()
            .int("cover-zoom")
            .clamp(MIN_COVER_ZOOM, MAX_COVER_ZOOM);
        imp.cover_zoom.set(step);
        imp.zoom_row.set_percent(cover_zoom_percent(step));

        // Past the width of the most columns with the largest covers, the
        // grid stays centered, rather than spreading the covers apart
        let (_, max_size) = grid_cover_sizes(step);
        let columns = max_columns(max_size);
        imp.grid_view.set_max_columns(columns);
        let grid_width = columns as i32 * (max_size + COVER_SPACING) + 2 * GRID_PADDING;
        imp.grid_clamp.set_maximum_size(grid_width);
        imp.grid_clamp.set_tightening_threshold(grid_width);

        imp.textures
            .borrow_mut()
            .set_capacity(cache_capacity(max_size));
        self.notify_cover_zoom();
        self.reset_covers();

        // Larger than the covers were made, they're made again, and until
        // then shown scaled up; back from a zoom in, they're made smaller.
        // The first scan makes them at the size they're zoomed to
        let made = imp.thumbnail_size.get();
        if made != 0 && thumbnail_sizes(step).update(Some(made)) != ThumbnailUpdate::Keep {
            self.start_scan(false);
        }

        let enable = |name: &str, on: bool| {
            if let Some(action) = imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(on);
            }
        };
        // The size between them stays on, so that it reads as the size
        enable("zoom-in", step < MAX_COVER_ZOOM);
        enable("zoom-out", step > MIN_COVER_ZOOM);

        // The columns change, and the album with the focus could go out
        // of view
        if let Some(position) =
            utils::focused_child::<AlbumCell>(&*imp.grid_view).and_then(|cell| cell.position())
        {
            utils::when_laid_out(&*imp.grid_view, move |grid| {
                grid.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
            });
        }
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
                    this.activate_album(&album, this.taken());
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
        crate::search::setup_query_entry(
            &imp.search_entry,
            crate::i18n::pi18n("a11y", "Type to search the library"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |problem| {
                    let banner = &this.imp().search_problem;
                    if let Some(problem) = problem {
                        banner.set_title(problem);
                    }
                    banner.set_revealed(problem.is_some());
                }
            ),
        );

        // Esc is the window's, in window/albums.rs
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

    // The music folders that aren't there or are left out, whose tracks
    // are hidden
    pub fn hidden_folders(&self) -> Vec<PathBuf> {
        self.imp().hidden_folders.borrow().clone()
    }

    // Whether the page shows the works, rather than the albums
    pub fn shows_works(&self) -> bool {
        self.imp().page.get() == LibraryPage::Works
    }

    // The Works page is only there while a track is part of a work, and
    // goes back to the albums once none is
    fn set_works_available(&self, available: bool) {
        let imp = self.imp();
        if imp.has_works.replace(available) != available {
            self.notify_has_works();
        }
        if !available && self.shows_works() {
            self.set_page(LibraryPage::Albums);
        }
        self.update_view();
    }

    // The works take the place of the grid, without its sorting, and its
    // title; the albums and the tracks share the grid, whose covers only
    // say how many of their tracks the search matches on the Tracks page,
    // as only those are taken there
    fn update_view(&self) {
        let imp = self.imp();
        let works = self.shows_works();
        let (title, search) = match imp.page.get() {
            LibraryPage::Albums => (i18n("Albums"), i18n("Search albums")),
            LibraryPage::Tracks => (i18n("Tracks"), i18n("Search tracks")),
            LibraryPage::Works => (i18n("Works"), i18n("Search works")),
        };
        imp.page_title.set_title(&title);
        imp.search_entry.set_placeholder_text(Some(&search));
        imp.search_entry
            .update_property(&[gtk::accessible::Property::Label(&search)]);
        imp.sort_button.set_visible(!works);
        // It names what it shows, for screen readers too
        let favorites = match imp.page.get() {
            LibraryPage::Albums => i18n("Show Only Favorite Albums"),
            LibraryPage::Tracks => i18n("Show Only Favorite Tracks"),
            LibraryPage::Works => i18n("Show Only Favorite Works"),
        };
        imp.favorites_button.set_tooltip_text(Some(&favorites));
        self.update_match_counts();
        self.update_select_button();
        self.update_selection();
        self.update_stack();
        self.update_works();
    }

    // The heart beside the search shows only the favorites: the albums on
    // the Albums page, the albums with favorite tracks on the Tracks page,
    // and the works on the Works page. It's filled and highlighted while it
    // does
    fn setup_favorites_filter(&self) {
        self.imp().favorites_button.connect_toggled(clone!(
            #[weak(rename_to = this)]
            self,
            move |button| {
                button.set_icon_name(if button.is_active() {
                    "heart-filled-symbolic"
                } else {
                    "heart-outline-symbolic"
                });
                // The works are read again, as for a favorite marked
                let generation = &this.imp().works_generation;
                generation.set(generation.get().wrapping_add(1));
                this.update_search();
                this.update_stack();
            }
        ));
    }

    // The album or the work that had the focus can go from the grid or the
    // list of works, as the last favorite unmarked from its menu does,
    // leaving the focus nowhere, and with it the page it was on; so the
    // stack and its pages drop the focus children that don't lead to the
    // window's focus each time it changes, see `utils::forget_focus_below`.
    // The lists then focus the item they last focused when the keys come in
    fn setup_lost_focus(&self) {
        self.connect_map(|this| {
            let imp = this.imp();
            if imp.follows_focus.get() {
                return;
            }
            let Some(window) = this.root().and_downcast::<gtk::Window>() else {
                return;
            };
            imp.follows_focus.set(true);
            window.connect_focus_widget_notify(clone!(
                #[weak]
                this,
                move |window| {
                    let focus = GtkWindowExt::focus(window);
                    let stack = this.imp().albums_stack.upcast_ref::<gtk::Widget>();
                    utils::forget_focus_below(stack, focus.as_ref());
                    let mut page = stack.first_child();
                    while let Some(child) = page {
                        utils::forget_focus_below(&child, focus.as_ref());
                        page = child.next_sibling();
                    }
                }
            ));
        });
    }

    // The lists of composers and of performers next to the search:
    // choosing one shows only their albums, with the tracks that are
    // theirs, like a search, and names them under the title of the page;
    // they combine with each other and with the search. All Composers or
    // All Performers, or Esc in an empty search, shows all the albums again
    fn setup_pickers(&self) {
        let imp = self.imp();
        self.setup_picker(
            Picker {
                popover: imp.composer_popover.get(),
                search: imp.composer_search.get(),
                list: imp.composer_list.get(),
                read: library::composers,
                others: |filter| TrackFilter {
                    composer: None,
                    ..filter
                },
                search_text: |composer: &library::ComposerInfo| &composer.search,
                pick: Self::set_composer,
            },
            None,
        );
        imp.composer_list.set_factory(Some(&composer_factory()));

        // The performers are in sections: the conductors, the groups and
        // the players, with All Performers first, in a section of its own
        let sections = gtk::CustomSorter::new(|a, b| {
            let kind = |item: &glib::Object| {
                let item = item.downcast_ref::<glib::BoxedAnyObject>().unwrap();
                let performer = item.borrow::<Option<library::PerformerInfo>>();
                performer.as_ref().map(|performer| performer.kind)
            };
            kind(a).cmp(&kind(b)).into()
        });
        self.setup_picker(
            Picker {
                popover: imp.performer_popover.get(),
                search: imp.performer_search.get(),
                list: imp.performer_list.get(),
                read: library::performers,
                others: |filter| TrackFilter {
                    performer: None,
                    ..filter
                },
                search_text: |performer: &library::PerformerInfo| &performer.search,
                pick: Self::set_performer,
            },
            Some(sections.upcast()),
        );
        imp.performer_list.set_factory(Some(&performer_factory()));
        imp.performer_list
            .set_header_factory(Some(&performer_section_factory()));
    }

    // Sets up a list to pick from: it's read each time it opens, as the
    // albums can change, with None, for all of them, first. It only has the
    // items with a track that the search and the other picks match, so that
    // a composer's list of performers has only theirs, and the other way
    // round, and its counts are of those albums; on the Works page, only
    // the tracks of works count, and the counts are of works. Its search
    // finds the items by their `search_text`, and Enter takes the first one
    // found. The `sections` sorter, if any, divides the list into sections,
    // which the list is already sorted by
    fn setup_picker<T: Clone + Send + 'static>(
        &self,
        picker: Picker<T>,
        sections: Option<gtk::Sorter>,
    ) {
        let Picker {
            popover,
            search,
            list,
            read,
            others,
            search_text,
            pick,
        } = picker;
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        // The words searched for, normalized once for all the items
        let words: Rc<RefCell<Vec<String>>> = Rc::default();
        let filter = gtk::CustomFilter::new(clone!(
            #[strong]
            words,
            move |item| {
                let words = words.borrow();
                let item = item.downcast_ref::<glib::BoxedAnyObject>().unwrap();
                match item.borrow::<Option<T>>().as_ref() {
                    // "All Composers" isn't something to search for
                    None => words.is_empty(),
                    Some(item) => {
                        let text = search_text(item);
                        words.iter().all(|word| text.contains(word.as_str()))
                    }
                }
            }
        ));
        let model: gio::ListModel = match sections {
            Some(sections) => {
                let sorted = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
                sorted.set_section_sorter(Some(&sections));
                sorted.upcast()
            }
            None => store.clone().upcast(),
        };
        let filtered = gtk::FilterListModel::new(Some(model), Some(filter.clone()));
        list.set_model(Some(&gtk::NoSelection::new(Some(filtered.clone()))));

        search.connect_search_changed(clone!(
            #[weak]
            filter,
            move |entry| {
                words.replace(
                    normalize_search_text(&entry.text())
                        .split_whitespace()
                        .map(ToString::to_string)
                        .collect(),
                );
                filter.changed(gtk::FilterChange::Different);
            }
        ));
        search.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            popover,
            #[weak]
            filtered,
            move |_| this.pick_from(&popover, &filtered, 0, pick)
        ));
        list.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            popover,
            #[weak]
            filtered,
            move |_, pos| this.pick_from(&popover, &filtered, pos, pick)
        ));

        popover.connect_show(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            store,
            #[weak]
            search,
            #[weak]
            list,
            move |_| {
                search.set_text("");
                search.grab_focus();
                let hidden = this.imp().hidden_folders.borrow().clone();
                let filter = others(this.wanted_filter());
                let counted = if this.shows_works() {
                    Counted::Works
                } else {
                    Counted::Albums
                };
                glib::spawn_future_local(clone!(
                    #[weak]
                    store,
                    #[weak]
                    list,
                    async move {
                        let items = gio::spawn_blocking(move || read(&hidden, &filter, counted))
                            .await
                            .unwrap_or_default();
                        let items: Vec<glib::BoxedAnyObject> = std::iter::once(None)
                            .chain(items.into_iter().map(Some))
                            .map(glib::BoxedAnyObject::new)
                            .collect();
                        store.splice(0, store.n_items(), &items);
                        // From the top, with All Composers, rather than
                        // where it was left, at the one picked. The search
                        // can have found none, if it was typed in while
                        // they were read
                        if list.model().is_some_and(|model| model.n_items() > 0) {
                            list.scroll_to(0, gtk::ListScrollFlags::NONE, None);
                        }
                    }
                ));
            }
        ));
    }

    fn pick_from<T: Clone + 'static>(
        &self,
        popover: &gtk::Popover,
        list: &gtk::FilterListModel,
        pos: u32,
        pick: fn(&Self, Option<T>),
    ) {
        let Some(item) = list.item(pos).and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let picked = item.borrow::<Option<T>>().clone();
        popover.popdown();
        pick(self, picked);
        // The popover gives the focus back to its button as it closes. The
        // search takes it, as the albums picked are searched further there,
        // and Esc in it shows all the albums again
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = this)]
            self,
            move || this.focus_search()
        ));
    }

    // Shows only the albums of a composer, or of all of them
    fn set_composer(&self, composer: Option<library::ComposerInfo>) {
        self.imp().composer.replace(composer);
        self.update_picks();
        self.update_search();
    }

    // Shows only the albums of a performer, or of all of them
    fn set_performer(&self, performer: Option<library::PerformerInfo>) {
        self.imp().performer.replace(performer);
        self.update_picks();
        self.update_search();
    }

    // The Composers and Performers buttons are only there while a track
    // has a composer, or a performer. One picked goes with its button, so
    // that the albums aren't left filtered by a button that can't be seen
    fn set_pickers_available(&self, composers: bool, performers: bool) {
        let imp = self.imp();
        imp.composer_button.set_visible(composers);
        imp.performer_button.set_visible(performers);
        let composer = !composers && imp.composer.replace(None).is_some();
        let performer = !performers && imp.performer.replace(None).is_some();
        if composer || performer {
            self.update_picks();
            self.update_search();
        }
    }

    // Whether only the albums of a composer or a performer, or only the
    // favorites, are shown
    pub fn has_picks(&self) -> bool {
        self.has_composer_or_performer() || self.imp().favorites_button.is_active()
    }

    fn has_composer_or_performer(&self) -> bool {
        let imp = self.imp();
        imp.composer.borrow().is_some() || imp.performer.borrow().is_some()
    }

    // Shows the albums of all the composers and performers again, and not
    // only the favorites
    pub fn clear_picks(&self) {
        let imp = self.imp();
        imp.composer.replace(None);
        imp.performer.replace(None);
        self.update_picks();
        if imp.favorites_button.is_active() {
            // Which searches again
            imp.favorites_button.set_active(false);
        } else {
            self.update_search();
        }
    }

    // The composer and the performer whose albums are shown are under the
    // title of the page, and on the tooltips of their buttons, as a narrow
    // window leaves out the title; the buttons are highlighted
    fn update_picks(&self) {
        let imp = self.imp();
        let composer = imp.composer.borrow();
        let performer = imp.performer.borrow();
        // Once, if they're the same, as for a composer who plays
        let names = [
            composer.as_ref().map(|c| c.name.as_str()),
            performer
                .as_ref()
                .filter(|p| composer.as_ref().is_none_or(|c| c.key != p.key))
                .map(|p| p.name.as_str()),
        ];
        imp.page_title
            .set_subtitle(&names.into_iter().flatten().collect::<Vec<_>>().join(" · "));

        imp.composer_button
            .set_tooltip_text(Some(&match composer.as_ref() {
                Some(composer) => i18n_k(
                    // Translators: `{composer}` must be left unmodified; it is
                    // expanded to the name of the composer whose albums are
                    // shown
                    "Composers: {composer}",
                    &[("composer", &composer.name)],
                ),
                None => i18n("Composers"),
            }));
        imp.performer_button
            .set_tooltip_text(Some(&match performer.as_ref() {
                Some(performer) => i18n_k(
                    // Translators: `{performer}` must be left unmodified; it is
                    // expanded to the name of the conductor, the group or the
                    // player whose albums are shown
                    "Performers: {performer}",
                    &[("performer", &performer.name)],
                ),
                None => i18n("Performers"),
            }));
        for (button, picked) in [
            (&imp.composer_button, composer.is_some()),
            (&imp.performer_button, performer.is_some()),
        ] {
            if picked {
                button.add_css_class("filtering");
            } else {
                button.remove_css_class("filtering");
            }
        }
    }

    // Shows all the albums and the works again, without the words of the
    // search, the composer and the performer picked, and the heart, as to
    // go to one they hide. Turning off the heart reads the works again
    fn clear_search(&self) {
        let imp = self.imp();
        imp.search_entry.set_text("");
        imp.composer.replace(None);
        imp.performer.replace(None);
        self.update_picks();
        imp.favorites_button.set_active(false);
        self.set_search_matches(TrackFilter::default(), None);
    }

    // What the search, the composer and performer picked, and the heart
    // ask for. On the Works page, the heart's works are kept from those
    // read, see `update_works`
    fn wanted_filter(&self) -> TrackFilter {
        let imp = self.imp();
        let favorites = if self.heart_filters(LibraryPage::Albums) {
            Some(Favorites::Albums(
                imp.favorite_albums.borrow().iter().cloned().collect(),
            ))
        } else if self.heart_filters(LibraryPage::Tracks) {
            Some(Favorites::Tracks(self.favorite_tracks()))
        } else {
            None
        };
        let query = TrackFilter::query(&imp.search_entry.text());
        let needs = query
            .as_ref()
            .map(|query| query.needs())
            .unwrap_or_default();
        // A folder that came through the document portal is read at a path
        // inside the sandbox, which `path:` doesn't look into
        let sources = needs.path.then(|| {
            Arc::new(library::SearchSources {
                shown_folders: library::music_folders(self.app_settings())
                    .into_iter()
                    .filter_map(|folder| Some((folder.path, folder.host?)))
                    .collect(),
            })
        });
        TrackFilter {
            query,
            sources,
            composer: imp.composer.borrow().as_ref().map(|c| c.key.clone()),
            performer: imp.performer.borrow().as_ref().map(|p| p.key.clone()),
            works: Vec::new(),
            favorites,
        }
    }

    // Whether the heart shows only the favorites of `page`, which is the
    // one shown
    fn heart_filters(&self, page: LibraryPage) -> bool {
        let imp = self.imp();
        imp.favorites_button.is_active() && imp.page.get() == page
    }

    // The paths of the favorite tracks. They're read on the main thread,
    // where they're marked, the first time the heart needs them, and kept
    // until one is marked or unmarked, see `favorite_tracks_changed()`
    fn favorite_tracks(&self) -> Arc<[PathBuf]> {
        self.imp()
            .favorite_tracks
            .borrow_mut()
            .get_or_insert_with(|| favorite_track_paths(&play_stats::favorites()))
            .clone()
    }

    // Follows tracks marked as favorites or unmarked, from their menu or
    // with the player's heart: the albums the heart shows on the Tracks
    // page change. An album's tracks open in the pane stay as they were,
    // as they do for a new search
    pub fn favorite_tracks_changed(&self) {
        self.imp().favorite_tracks.replace(None);
        // A search for the favorites finds them, or leaves them out
        let searched = TrackFilter::query(&self.imp().search_entry.text())
            .is_some_and(|query| query.needs().favorites);
        if searched {
            self.update_search();
        } else if self.heart_filters(LibraryPage::Tracks) {
            self.update_album_matches();
        }
    }

    // Looks for the albums with a track that the search matches, like
    // searching the queue, and that's by the composer picked, with the
    // performer picked, if any, and among the favorites while the heart is
    // on; and for the works
    fn update_search(&self) {
        self.update_works();
        self.update_album_matches();
    }

    // Looks for the albums of the search, see `update_search`
    fn update_album_matches(&self) {
        let filter = self.wanted_filter();
        if filter.is_empty() {
            self.set_search_matches(TrackFilter::default(), None);
            return;
        }

        let hidden = self.imp().hidden_folders.borrow().clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok((filter, matches)) = gio::spawn_blocking(move || {
                    let matches = library::search_albums(&filter, &hidden);
                    (filter, matches)
                })
                .await
                else {
                    return;
                };

                // Results for an older search arriving late
                if this.wanted_filter() != filter {
                    return;
                }
                this.set_search_matches(filter, Some(matches));
            }
        ));
    }

    fn set_search_matches(
        &self,
        filter: TrackFilter,
        matches: Option<HashMap<String, (u32, u32)>>,
    ) {
        let imp = self.imp();
        imp.search_filter.replace(filter);
        imp.search_matches.replace(matches);
        self.update_match_counts();
        self.update_selection();
        if let Some(filter) = self.imp().filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
        self.update_stack();
    }

    // The covers of the albums the search matches only partly say how many
    // of their tracks it does, on the page that takes only those
    fn update_match_counts(&self) {
        let imp = self.imp();
        let matches = imp.search_matches.borrow();
        let matches = matches.as_ref().filter(|_| imp.page.get().takes_matches());
        let Some(model) = imp.model.borrow().clone() else {
            return;
        };
        for album in model.iter::<Album>().flatten() {
            let (matching, total) = matches
                .and_then(|matches| matches.get(album.id_str()).copied())
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

    // What the tracks taken from an album of the grid have to match, to be
    // added, played, dragged or shown, and the names of the composer and the
    // performer it picks: on the Tracks page, the words of the search and
    // the composer and the performer picked; on the Albums page, nothing,
    // as they're taken whole; on the Works page, the open work's tracks
    pub fn taken(&self) -> (TrackFilter, PickNames) {
        match self.imp().page.get() {
            LibraryPage::Albums => Default::default(),
            LibraryPage::Tracks => (self.search_filter(), self.search_pick_names()),
            LibraryPage::Works => self.taken_from_open_work(),
        }
    }

    // What's taken from the album of a cell: among the covers of the albums
    // of a work, its tracks, and otherwise as for the grid, see `taken()`
    pub fn taken_by(&self, cell: &AlbumCell) -> (TrackFilter, PickNames) {
        match cell.work() {
            Some(work) => works::taken_from_work(work.keys, work.name),
            None => self.taken(),
        }
    }

    // The words of the search, and the composer and the performer picked,
    // that the albums shown were found with
    fn search_filter(&self) -> TrackFilter {
        self.imp().search_filter.borrow().clone()
    }

    // The names of the composer and the performer in `search_filter()`, if
    // any. The ones picked can differ for a moment, while the albums of new
    // ones are read
    fn search_pick_names(&self) -> PickNames {
        let imp = self.imp();
        let filter = imp.search_filter.borrow();
        PickNames {
            composer: imp
                .composer
                .borrow()
                .as_ref()
                .filter(|composer| filter.composer.as_ref() == Some(&composer.key))
                .map(|composer| composer.name.clone()),
            performer: imp
                .performer
                .borrow()
                .as_ref()
                .filter(|performer| filter.performer.as_ref() == Some(&performer.key))
                .map(|performer| performer.name.clone()),
            work: None,
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
            .find(|a| a.id_str() == album_id)
    }

    // Whether an album is marked as a favorite
    pub fn is_favorite_album(&self, album_id: &str) -> bool {
        self.imp().favorite_albums.borrow().contains(album_id)
    }

    // Follows albums marked as favorites or unmarked: the hearts of their
    // covers, and for those marked, the first track, which finds an album
    // again once a change to its tags gives it another id. That's looked up
    // in the library in another thread, so that marking doesn't wait for it
    pub fn favorite_albums_changed(&self, ids: &[String], favorite: bool) {
        {
            let mut favorites = self.imp().favorite_albums.borrow_mut();
            for id in ids {
                if favorite {
                    favorites.insert(id.clone());
                } else {
                    favorites.remove(id);
                }
            }
        }
        self.apply_favorite_albums();
        // The albums the heart shows changed
        if self.heart_filters(LibraryPage::Albums) {
            self.update_album_matches();
        }
        if !favorite {
            return;
        }

        let ids = ids.to_vec();
        glib::spawn_future_local(async move {
            let Ok(uris) = gio::spawn_blocking(move || {
                ids.into_iter()
                    .filter_map(|id| {
                        let uri = first_track_uri(&id)?;
                        Some((id, uri))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            else {
                return;
            };
            play_stats::update_favorite_albums(&play_stats::FavoriteAlbumUpdates {
                uris,
                ..Default::default()
            });
        });
    }

    // Shows the hearts of the favorite albums of the grid
    fn apply_favorite_albums(&self) {
        let imp = self.imp();
        let favorites = imp.favorite_albums.borrow();
        let Some(model) = imp.model.borrow().clone() else {
            return;
        };
        for album in model.iter::<Album>().flatten() {
            let favorite = favorites.contains(album.id_str());
            if album.favorite() != favorite {
                album.set_favorite(favorite);
            }
        }
    }

    // Scrolls the grid to an album, clearing the search if it hides it
    pub fn reveal_album(&self, album_id: &str) -> Option<Album> {
        let imp = self.imp();
        let album = self.album(album_id)?;

        // The heart hides the albums that aren't favorites, whose matches
        // can still be read, as when it's gone to from the Works page. Gone
        // to from the Tracks page, the matches are still those of its
        // favorite tracks, so a favorite album without one clears the heart,
        // though the Albums page would show it
        let hidden = imp
            .search_matches
            .borrow()
            .as_ref()
            .is_some_and(|m| !m.contains_key(album_id))
            || (self.heart_filters(LibraryPage::Albums) && !self.is_favorite_album(album_id));
        if hidden {
            self.clear_search();
        }

        let model = imp.grid_view.model()?;
        let pos = (0..model.n_items())
            .find(|&i| model.item(i).and_downcast::<Album>().as_ref() == Some(&album))?;
        imp.grid_view
            .scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);

        Some(album)
    }

    // Scrolls the grid to an album, which takes the focus, and opens it, as
    // a click on its cover does; the grid was just shown, from the covers of
    // a work, see `utils::focus_when_laid_out`
    pub fn open_album(&self, album_id: &str) {
        let Some(album) = self.reveal_album(album_id) else {
            return;
        };
        self.activate_album(&album, self.taken());
        utils::focus_when_laid_out(&*self.imp().grid_view);
    }

    // Puts the cursor in the search, so that typing searches right away.
    // With no album to search, the button of the empty page takes the
    // focus instead, so that Enter goes on to choose the music folders;
    // typing still goes to the search, through the window
    pub fn focus_search(&self) {
        let imp = self.imp();
        if imp.albums_stack.visible_child_name().as_deref() == Some("empty") {
            imp.empty_button.grab_focus();
        } else {
            imp.search_entry.grab_focus();
        }
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
            ids.remove(album.id_str());
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

    // The albums that can be selected, in their order: those the grid
    // shows, or on the Works page those of the covers of the open work
    fn selectable_albums(&self) -> Vec<Album> {
        let imp = self.imp();
        if self.shows_works() {
            return self.open_work_albums();
        }
        let Some(model) = imp.grid_view.model() else {
            return Vec::new();
        };
        // The items of the selection model are typed as GObject, so
        // `iter::<Album>()` would panic
        (0..model.n_items())
            .filter_map(|i| model.item(i).and_downcast::<Album>())
            .collect()
    }

    // Selects the albums from the last one clicked to this one, in the
    // order of the grid; without a last one, it toggles the album like a
    // click
    fn select_range(&self, album: &Album) {
        let anchor = self.imp().selection_anchor.upgrade();
        let albums = self.selectable_albums();
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

    // Selects all the albums shown by the search, or the covers of the open
    // work; on the Works page, there's nothing to select until one is open
    pub fn select_all(&self) {
        let albums = self.selectable_albums();
        if self.shows_works() && albums.is_empty() {
            return;
        }
        self.set_selection_mode(true);
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

    // The selected albums, in the order of the grid or of the covers, with
    // what their tracks have to match to be taken
    pub fn selected_albums(&self) -> Vec<(String, TrackFilter)> {
        let filter = self.taken().0;
        self.selectable_albums()
            .into_iter()
            .filter(Album::selected)
            .map(|album| (album.id(), filter.clone()))
            .collect()
    }

    // The selected albums, leaving selection mode, as the buttons of its
    // bar do once they've taken them
    pub fn take_selected_albums(&self) -> Vec<(String, TrackFilter)> {
        let albums = self.selected_albums();
        self.set_selection_mode(false);
        albums
    }

    // The menu next to the button that adds the albums, with the other ways
    // to add them
    pub fn set_add_menu(&self, menu: &gio::MenuModel) {
        self.imp().add_albums_button.set_menu_model(Some(menu));
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

    // On the Works page, the covers of the open work are selected, so
    // there's nothing to select until one is open
    fn update_select_button(&self) {
        let imp = self.imp();
        let selectable = !self.shows_works() || imp.expanded_work.borrow().is_some();
        imp.select_button.set_sensitive(selectable);
    }

    fn update_selection(&self) {
        let imp = self.imp();
        let selected = imp.selected.borrow();
        let n_selected = selected.len() as u32;
        // During a search on the Tracks page, and on the Works page, the
        // number of tracks the buttons take, as they take only some of each
        // album
        let matching = match imp.page.get() {
            LibraryPage::Albums => None,
            LibraryPage::Tracks => imp.search_matches.borrow().as_ref().map(|matches| {
                selected
                    .iter()
                    .filter_map(|id| matches.get(id))
                    .map(|(matching, _)| matching)
                    .sum()
            }),
            LibraryPage::Works => self.open_work_tracks(&selected),
        };
        drop(selected);
        let label = if n_selected == 0 {
            i18n("No album selected")
        } else {
            let albums = ni18n_f(
                // Translators: The '{}' must be left unmodified, and
                // it is expanded to the number of albums selected
                "{} album selected",
                "{} albums selected",
                n_selected,
                &[&n_selected.to_string()],
            );
            match matching {
                Some(n) => {
                    let tracks = ni18n_f(
                        // Translators: The '{}' must be left unmodified, and
                        // it is expanded to the number of tracks of the
                        // albums selected that the search matches, or that
                        // are of a work
                        "{} track",
                        "{} tracks",
                        n,
                        &[&n.to_string()],
                    );
                    format!("{albums} · {tracks}")
                }
                None => albums,
            }
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
                let (
                    hidden,
                    albums,
                    albums_unavailable,
                    has_composers,
                    has_performers,
                    has_works,
                    favorites,
                ) = gio::spawn_blocking(move || {
                    let roots = library::roots(&folders);
                    let hidden = [roots.unavailable.as_slice(), &roots.excluded].concat();
                    let albums = library::albums(&hidden);
                    // Only asked while there's no album to show: with
                    // only the folders left out hidden, the albums found
                    // are in the folders that are missing or empty
                    let albums_unavailable = albums.is_empty()
                        && !roots.unavailable.is_empty()
                        && library::has_albums(&roots.excluded);
                    let has_composers = library::has_composers(&hidden);
                    let has_performers = library::has_performers(&hidden);
                    let has_works = library::has_works(&hidden);
                    let shown = albums.iter().map(|album| album.id.as_str()).collect();
                    let favorites = favorite_album_updates(
                        &play_stats::favorite_albums(),
                        &shown,
                        album_of_uri,
                        first_track_uri,
                    );
                    (
                        hidden,
                        albums,
                        albums_unavailable,
                        has_composers,
                        has_performers,
                        has_works,
                        favorites,
                    )
                })
                .await
                .unwrap_or_default();
                if this.imp().reload_generation.get() == generation {
                    play_stats::update_favorite_albums(&favorites);
                    // Read again here, rather than kept from the other
                    // thread, which would leave out an album marked while
                    // the albums were read
                    this.imp()
                        .favorite_albums
                        .replace(play_stats::favorite_album_ids());
                    this.imp().hidden_folders.replace(hidden);
                    // Before the albums, which can switch to the page and
                    // give its button the focus, and also when there were
                    // no albums before either, as the grid then stays as it
                    // is, but the folders can have changed
                    this.imp().albums_unavailable.set(albums_unavailable);
                    this.update_empty_page();
                    this.show_albums(albums);
                    // Also when the albums are the same, and the grid keeps
                    // them
                    this.apply_favorite_albums();
                    this.set_pickers_available(has_composers, has_performers);
                    // The works are read again, as the tracks or the hidden
                    // folders can have changed
                    let works_generation = &this.imp().works_generation;
                    works_generation.set(works_generation.get().wrapping_add(1));
                    this.set_works_available(has_works);
                }
            }
        ));
    }

    fn show_albums(&self, albums: Vec<library::AlbumInfo>) {
        let imp = self.imp();

        // After a scan that changed nothing, as at most starts, the albums
        // read again are the same, and the grid stays as it is, rather
        // than read the covers it shows again. So does it after tags were
        // changed in a way the grid doesn't show, as when a tagger saves
        // an album, though a search can find other tracks now
        let covers_changed = imp.covers_changed.take();
        let tracks_changed = imp.tracks_changed.take();
        if !covers_changed && *imp.shown_albums.borrow() == albums {
            if tracks_changed && imp.search_matches.borrow().is_some() {
                self.update_search();
            }
            return;
        }
        imp.shown_albums.replace(albums.clone());

        let albums: Vec<Album> = albums.into_iter().map(Album::new).collect();
        debug!("Library: {} albums", albums.len());

        // Keep the selection, minus the albums that are gone
        {
            let mut selected = imp.selected.borrow_mut();
            let ids: HashSet<&str> = albums.iter().map(Album::id_str).collect();
            selected.retain(|id| ids.contains(id.as_str()));
            for album in &albums {
                album.set_selected(selected.contains(album.id_str()));
            }
        }
        self.update_selection();

        if let Some(model) = imp.model.borrow().as_ref() {
            model.splice(0, model.n_items(), &albums);
        }

        // A cover made again can keep its path; otherwise, the covers in
        // the cache are still the ones of these albums
        if covers_changed {
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
        let page = if n_albums > 0 && self.finds_no_favorites() {
            self.update_no_favorites_page();
            "no-favorites"
        } else if n_albums > 0 && self.shows_works() {
            "works"
        } else if n_albums > 0 {
            "albums"
        } else if imp.scanning.get() {
            "scanning"
        } else {
            "empty"
        };
        let stack = &*imp.albums_stack;
        if stack.visible_child_name().as_deref() != Some(page) {
            // The empty page of the heart has nothing to focus, so the focus
            // goes to the search beside the heart, when it was on the page
            // that goes, or on an album that went just before, as the last
            // favorite unmarked from its menu does. That leaves it nowhere,
            // or on the item of the menu, which is hidden then
            let focus = self.root().and_then(|root| root.focus());
            let lost = focus
                .as_ref()
                .is_none_or(|focus| focus.is_ancestor(stack) || !focus.is_drawable());
            // With no album at all, there's nothing to search either, so
            // the button that chooses the music folders also takes the
            // focus from an empty search, which had it at first run while
            // the folders were read
            let in_empty_search = focus
                .as_ref()
                .is_some_and(|focus| focus.is_ancestor(&*imp.search_entry))
                && imp.search_entry.text().is_empty();
            stack.set_visible_child_name(page);
            if lost && page == "no-favorites" {
                imp.search_entry.grab_focus();
            } else if page == "empty" && (lost || in_empty_search) && self.is_drawable() {
                imp.empty_button.grab_focus();
            }
        }

        // When the grid is already showing, a thin bar at the bottom is
        // enough
        imp.update_progress
            .set_visible(n_albums > 0 && imp.scanning.get());
    }

    // Whether the heart beside the search finds nothing on the page, once
    // what it finds is read: there are no favorites, or none that the rest
    // of the search matches. Just after the Albums and Tracks pages switch,
    // the matches are still those of the other page's favorites
    fn finds_no_favorites(&self) -> bool {
        let imp = self.imp();
        if !imp.favorites_button.is_active() {
            return false;
        }
        if self.shows_works() {
            imp.works_filter.borrow().is_some()
                && imp
                    .works
                    .borrow()
                    .as_ref()
                    .is_some_and(|works| works.n_items() == 0)
        } else {
            *imp.search_filter.borrow() == self.wanted_filter()
                && imp
                    .search_matches
                    .borrow()
                    .as_ref()
                    .is_some_and(HashMap::is_empty)
        }
    }

    // The page without albums says where they were looked for, or that the
    // music folders that have them are missing or empty. The button, which
    // takes the focus, has the same description, for screen readers
    fn update_empty_page(&self) {
        let imp = self.imp();
        let folders: Vec<PathBuf> = library::music_folders(self.app_settings())
            .iter()
            .map(|folder| folder.shown_path().to_path_buf())
            .collect();
        let (title, description) = empty_page_text(&folders, imp.albums_unavailable.get());
        imp.empty_page.set_title(&title);
        // It's markup, and the name of a folder can have a "&" in it
        imp.empty_page
            .set_description(Some(&glib::markup_escape_text(&description)));
        imp.empty_button
            .update_property(&[gtk::accessible::Property::Description(&description)]);
    }

    // The page says so, and how to mark favorites if there are none
    fn update_no_favorites_page(&self) {
        let imp = self.imp();
        let searched =
            !imp.search_entry.text().trim().is_empty() || self.has_composer_or_performer();
        let (title, description) = match (imp.page.get(), searched) {
            (LibraryPage::Albums, false) => (
                i18n("No Favorite Albums"),
                i18n(
                    "Mark an album as a favorite from its menu, or with the heart over its tracks",
                ),
            ),
            (LibraryPage::Tracks, false) => (
                i18n("No Favorite Tracks"),
                i18n(
                    "Mark a track as a favorite from its menu, or with the heart next to the sleep timer",
                ),
            ),
            (LibraryPage::Works, false) => (
                i18n("No Favorite Works"),
                i18n("Mark a work or one of its recordings as a favorite from its menu"),
            ),
            (_, true) => (
                i18n("No Results Found"),
                match imp.page.get() {
                    LibraryPage::Albums => i18n("None of the favorite albums match the search"),
                    LibraryPage::Tracks => i18n("None of the favorite tracks match the search"),
                    LibraryPage::Works => i18n("None of the favorite works match the search"),
                },
            ),
        };
        imp.no_favorites_page.set_title(&title);
        imp.no_favorites_page.set_description(Some(&description));
    }

    // Files that come, go or change in the music folders are read once
    // they stop changing, as at startup
    fn setup_watch(&self) {
        let watch = library::FolderWatch::new(clone!(
            #[weak(rename_to = this)]
            self,
            move || this.start_scan(false)
        ));
        self.imp().watch.set(watch).unwrap();
    }

    // A music folder that can't be watched is read again when the albums
    // are shown, unless it's being read already, or was read in the last
    // few minutes
    pub fn scan_unwatched(&self) {
        let imp = self.imp();
        let recent = imp
            .last_scan_finished
            .get()
            .is_some_and(|finished| finished.elapsed() < UNWATCHED_RESCAN_INTERVAL);
        if imp.unwatched.get() && !imp.scanning.get() && !recent {
            self.start_scan(false);
        }
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
            Duration::from_millis(100),
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
        let thumbnail_sizes = thumbnail_sizes(imp.cover_zoom.get());
        imp.thumbnail_size.set(thumbnail_sizes.wanted);
        std::thread::spawn(move || library::scan(&folders, thumbnail_sizes, &sender));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let mut result = ScanResult::default();
                let mut finished = false;
                while let Ok(event) = receiver.recv().await {
                    let imp = this.imp();
                    match event {
                        ScanEvent::Progress(done, total) | ScanEvent::Covers(done, total) => {
                            counting.set(true);
                            let fraction = done as f64 / total.max(1) as f64;
                            let (done, total) = (done.to_string(), total.to_string());
                            let numbers = [("done", done.as_str()), ("total", total.as_str())];
                            let text = if matches!(event, ScanEvent::Covers(..)) {
                                i18n_k(
                                    // Translators: the `{done}` and `{total}` must be left
                                    // unmodified; they are the number of album covers made
                                    // so far and the number of covers to make
                                    "{done} of {total} covers",
                                    &numbers,
                                )
                            } else {
                                i18n_k(
                                    // Translators: the `{done}` and `{total}` must be left
                                    // unmodified; they are the number of tracks read so far
                                    // and the number of tracks to read
                                    "{done} of {total} tracks",
                                    &numbers,
                                )
                            };
                            imp.scan_progress.set_fraction(fraction);
                            imp.scan_progress.set_text(Some(&text));
                            imp.update_progress.set_fraction(fraction);
                        }
                        ScanEvent::Finished(scanned) => {
                            result = scanned;
                            finished = true;
                            break;
                        }
                    }
                }

                // The progress bars stop pulsing for this scan, even when
                // another one follows. The channel also closes if the scan
                // panicked
                counting.set(true);
                this.imp().last_scan_finished.set(Some(Instant::now()));
                if result.covers_changed {
                    this.imp().covers_changed.set(true);
                }
                if result.n_changed > 0 {
                    this.imp().tracks_changed.set(true);
                }
                let uris: Vec<String> = result
                    .updated
                    .iter()
                    .map(|path| gio::File::for_path(path).uri().to_string())
                    .collect();
                let n_changed = u32::try_from(result.n_changed).unwrap_or(u32::MAX);

                // The folders the scan read are watched from now on. Files
                // that came into a new one before it was watched are read
                // by another scan. A scan that panicked doesn't say
                if finished {
                    let imp = this.imp();
                    imp.unwatched.set(result.watch.unwatched);
                    if imp
                        .watch
                        .get()
                        .is_some_and(|watch| watch.set_folders(result.watch))
                    {
                        imp.rescan_pending.set(true);
                    }
                }

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

// A list next to the search, to pick a composer or a performer from, see
// `LibraryView::setup_picker()`
struct Picker<T> {
    popover: gtk::Popover,
    search: gtk::SearchEntry,
    list: gtk::ListView,
    // Reads the items, in another thread, without the hidden folders,
    // those of the tracks the filter matches, with the number of albums,
    // or of works on the Works page
    read: fn(&[PathBuf], &TrackFilter, Counted) -> Vec<T>,
    // The filter of the items that go with the others picked and the
    // search: the one of the albums shown, without the item picked
    others: fn(TrackFilter) -> TrackFilter,
    // The text, normalized, that the list's search looks into
    search_text: fn(&T) -> &str,
    // Shows the albums of the item picked, or of all of them
    pick: fn(&LibraryView, Option<T>),
}

// The names of the composer and the performer whose albums are shown, if
// any, or of the work whose tracks are taken from an album, which the bar
// over an album's tracks says
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickNames {
    pub composer: Option<String>,
    pub performer: Option<String>,
    pub work: Option<String>,
}

// The line of a row of the lists of composers, performers and works: a
// name, which the end of a long row cuts off, and a number. The scrollbar
// is drawn over the right edge of the list. The line is as high as the
// theme makes a row at least, so that its text is in the middle of the row
// rather than at its top, and the line of an open work, with its covers
// below it, is as high (style.css)
fn fill_name_count_line(line: &gtk::Box, name: &gtk::Label, count: &gtk::Label) {
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    count.add_css_class("dim-label");
    count.add_css_class("numeric");
    line.set_spacing(12);
    line.set_margin_end(12);
    line.set_vexpand(true);
    line.add_css_class("row-line");
    line.append(name);
    line.append(count);
}

// The rows of the lists of composers and performers: a line with a name
// and a number, which `bind` sets from the `T` of the item, a
// `BoxedAnyObject`; `tooltip` makes the line's tooltip from it when it
// shows. Their line is in a box, as a work's is in its row
fn name_count_factory<T: 'static>(
    tooltip: fn(&T) -> Option<String>,
    bind: fn(&T, &gtk::Label, &gtk::Label),
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (line, name, count) = Default::default();
        fill_name_count_line(&line, &name, &count);
        utils::set_row_tooltip(&line, item, move |object| {
            tooltip(&object.downcast_ref::<glib::BoxedAnyObject>()?.borrow::<T>())
        });
        // Set, so that the line's expanding doesn't spread to the list
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(false)
            .build();
        row.append(&line);
        item.set_child(Some(&row));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(line) = item.child().and_then(|row| row.first_child()) else {
            return;
        };
        let (Some(name), Some(count)) = (
            line.first_child().and_downcast::<gtk::Label>(),
            line.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        if let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() {
            bind(&object.borrow::<T>(), &name, &count);
        }
    });
    factory
}

// A name followed, dimmed, by more about it, if there's more, as markup
fn with_dimmed(name: &str, more: &str) -> String {
    let name = glib::markup_escape_text(name);
    if more.is_empty() {
        name.to_string()
    } else {
        let more = glib::markup_escape_text(more);
        format!("{name}  <span alpha=\"55%\">{more}</span>")
    }
}

// The rows of the list of composers: the name they're sorted by, and the
// number of their albums
fn composer_factory() -> gtk::SignalListItemFactory {
    name_count_factory::<Option<library::ComposerInfo>>(
        // The name it's known by, as the row has the one it's sorted by
        |composer| composer.as_ref().map(|composer| composer.name.clone()),
        |composer, name, count| {
            if let Some(composer) = composer {
                name.set_label(&composer.label);
                count.set_label(&composer.count.to_string());
            } else {
                name.set_label(&i18n("All Composers"));
                count.set_label("");
            }
        },
    )
}

// The rows of the list of performers: the name they're sorted by, then,
// dimmed, what they play or sing, which the end of a long row cuts off
// first, and the number of their albums
fn performer_factory() -> gtk::SignalListItemFactory {
    name_count_factory::<Option<library::PerformerInfo>>(
        // The name they're known by, and all they play or sing
        |performer| {
            let performer = performer.as_ref()?;
            let roles = performer.roles.join(", ");
            let text = [performer.name.as_str(), roles.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            Some(text)
        },
        |performer, name, count| {
            if let Some(performer) = performer {
                name.set_markup(&with_dimmed(&performer.sort, &performer.roles.join(", ")));
                count.set_label(&performer.count.to_string());
            } else {
                name.set_label(&i18n("All Performers"));
                count.set_label("");
            }
        },
    )
}

// The headers of the sections of the list of performers; All Performers,
// at the top, has none
fn performer_section_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["caption-heading", "dim-label", "picker-section"])
            .build();
        header.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
        let Some(label) = header.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let kind = header
            .item()
            .and_downcast::<glib::BoxedAnyObject>()
            .and_then(|item| {
                item.borrow::<Option<library::PerformerInfo>>()
                    .as_ref()
                    .map(|performer| performer.kind)
            });
        label.set_visible(kind.is_some());
        label.set_label(&match kind {
            Some(PerformerKind::Conductor) => i18n("Conductors"),
            Some(PerformerKind::Ensemble) => i18n("Ensembles"),
            Some(PerformerKind::Soloist) => i18n("Soloists"),
            None => String::new(),
        });
    });
    factory
}

// What follows the favorite albums through changes to the library: the
// albums whose ids changed, and the first tracks of the albums marked since
// they were last looked up. An album whose id isn't among the `shown` ones
// has another id if its first track, by `album_of_uri`, does; if that's the
// same, the album is only hidden, as on a drive that isn't mounted.
// `first_track_uri` gives the URI of an album's first track, by its id
fn favorite_album_updates(
    favorites: &[play_stats::FavoriteAlbum],
    shown: &HashSet<&str>,
    album_of_uri: impl Fn(&str) -> Option<String>,
    first_track_uri: impl Fn(&str) -> Option<String>,
) -> play_stats::FavoriteAlbumUpdates {
    let mut updates = play_stats::FavoriteAlbumUpdates::default();
    for favorite in favorites {
        match &favorite.uri {
            None => {
                if let Some(uri) = first_track_uri(&favorite.id) {
                    updates.uris.push((favorite.id.clone(), uri));
                }
            }
            Some(uri) if !shown.contains(favorite.id.as_str()) => {
                if let Some(id) = album_of_uri(uri).filter(|id| *id != favorite.id) {
                    updates.moves.push((favorite.id.clone(), id));
                }
            }
            Some(_) => {}
        }
    }
    updates
}

// The paths of the favorite tracks, from their URIs, as the library keeps
// them; a track of a CUE sheet has its path under the sheet in both
fn favorite_track_paths(uris: &[String]) -> Arc<[PathBuf]> {
    uris.iter()
        .filter_map(|uri| gio::File::for_uri(uri).path())
        .collect()
}

// The album of the track with a URI, in the library, whether its folder is
// hidden or not; this blocks
fn album_of_uri(uri: &str) -> Option<String> {
    let path = gio::File::for_uri(uri).path()?;
    library::track_album(&path, &[])
}

// The URI of the first track of an album, whether its folder is hidden or
// not; this blocks
fn first_track_uri(album_id: &str) -> Option<String> {
    let path = library::album_tracks(album_id, &TrackFilter::default(), &[])
        .into_iter()
        .next()?;
    Some(gio::File::for_path(path).uri().to_string())
}

// What a table of the covers' sizes has at a step of the zoom
fn at_zoom_step<T: Copy>(sizes: &[T], step: i32) -> T {
    let index = usize::try_from(step - MIN_COVER_ZOOM).unwrap_or(0);
    sizes[index.min(sizes.len() - 1)]
}

// The sizes the covers of the grid grow between at a step of the zoom
fn grid_cover_sizes(step: i32) -> (i32, i32) {
    at_zoom_step(&COVER_SIZES, step)
}

// The size of the covers at a step of the zoom, in percent of the normal
// size, by their largest size
fn cover_zoom_percent(step: i32) -> u32 {
    let ((_, max), (_, normal)) = (grid_cover_sizes(step), grid_cover_sizes(0));
    (f64::from(max) * 100.0 / f64::from(normal)).round() as u32
}

// The most columns the grid has with covers of this largest size, so that
// it's never wider than MAX_GRID_WIDTH
fn max_columns(max_size: i32) -> u32 {
    (f64::from(MAX_GRID_WIDTH) / f64::from(max_size + COVER_SPACING)).round() as u32
}

// How many covers the cache keeps with covers of this largest size: about
// as many pixels as at the normal size
fn cache_capacity(max_size: i32) -> usize {
    let (_, normal) = grid_cover_sizes(0);
    let ratio = f64::from(normal) / f64::from(max_size);
    (MAX_CACHED_COVERS as f64 * ratio * ratio).round() as usize
}

// The cells under the widget that show an album's cover, the grid's and
// those of an open work, which are mapped; GridView keeps the cells off
// screen bound, but unmapped
fn mapped_album_cells(widget: &gtk::Widget) -> Vec<AlbumCell> {
    let mut cells = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        match widget.downcast_ref::<AlbumCell>() {
            Some(cell) if cell.picture().is_mapped() => cells.push(cell.clone()),
            Some(_) => {}
            None if widget.is_mapped() => cells.extend(mapped_album_cells(&widget)),
            None => {}
        }
        child = widget.next_sibling();
    }
    cells
}

// The widths the covers of the grid are made at: the largest the grid shows
// them, in the pixels of the monitor with the largest scale, and at a scale
// of 2, which the scan keeps (`ThumbnailSizes::update`). A cover twice as
// large as it's shown takes four times the memory, and the time to read.
// Zoomed out, they're made at the normal size, as they're scaled down as
// they're read, and zooming back in would make them all again
fn thumbnail_sizes(zoom: i32) -> library::ThumbnailSizes {
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
    let (_, max_size) = grid_cover_sizes(zoom.max(0));
    library::ThumbnailSizes {
        wanted: (max_size * scale) as u32,
        scale_2: (max_size * 2) as u32,
    }
}

// The copy of an album that follows the pointer while it's dragged: its
// cover, with the number of albums when there are several
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

// The title and the description of the page without albums: where they
// were looked for, naming the music folder if there's one, or that the
// music folders that have them are missing or empty, as when their drive
// isn't plugged in
fn empty_page_text(folders: &[PathBuf], albums_unavailable: bool) -> (String, String) {
    let folder = match folders {
        [folder] => Some(utils::home_path(folder)),
        _ => None,
    };
    match (albums_unavailable, folder) {
        (false, Some(folder)) => (
            i18n("No Albums"),
            i18n_k(
                // Translators: the `{folder}` must be left unmodified; it's
                // the music folder, e.g. ~/Music
                "Dismas looked for albums in {folder} and found none. Choose the folders your music is in.",
                &[("folder", &folder)],
            ),
        ),
        (false, None) => (
            i18n("No Albums"),
            i18n(
                "Dismas found no albums in your music folders. Choose the folders your music is in.",
            ),
        ),
        (true, Some(folder)) => (
            i18n("Music Folder Not Available"),
            i18n_k(
                // Translators: the `{folder}` must be left unmodified; it's
                // the music folder, e.g. ~/Music
                "{folder} is missing or empty, so its albums are hidden. Connect the drive it's on and they come back by themselves, or choose another folder.",
                &[("folder", &folder)],
            ),
        ),
        (true, None) => (
            i18n("Music Folders Not Available"),
            i18n(
                "The music folders with your albums are missing or empty, so the albums are hidden. Connect the drives they're on and the albums come back by themselves, or choose other folders.",
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_zoom_step_changes_the_size_of_the_covers() {
        // A step starts where the one before ends, so the covers change
        // size even where the grid keeps its columns
        for sizes in [&COVER_SIZES, &works::WORK_COVER_SIZES] {
            assert_eq!(sizes.len(), (MAX_COVER_ZOOM - MIN_COVER_ZOOM + 1) as usize);
            for pair in sizes.windows(2) {
                let ((min, max), (next_min, _)) = (pair[0], pair[1]);
                assert!(min < max && max <= next_min, "{sizes:?}");
            }
        }
        assert_eq!(grid_cover_sizes(0), (160, 240));
        assert_eq!(grid_cover_sizes(MIN_COVER_ZOOM), (90, 120));
        assert_eq!(grid_cover_sizes(MAX_COVER_ZOOM), (240, 320));
        // A step out of range, from a hand edit of the setting
        assert_eq!(grid_cover_sizes(9), (240, 320));
    }

    #[test]
    fn smaller_covers_have_more_columns_and_more_in_the_cache() {
        let columns: Vec<_> = COVER_SIZES
            .iter()
            .map(|&(_, max)| max_columns(max))
            .collect();
        assert_eq!(columns, [11, 9, 6, 5]);
        let cached: Vec<_> = COVER_SIZES
            .iter()
            .map(|&(_, max)| cache_capacity(max))
            .collect();
        assert_eq!(cached, [600, 338, 150, 84]);
    }

    #[test]
    fn empty_page_names_the_folder() {
        let music = glib::home_dir().join("Music");
        let (title, description) = empty_page_text(std::slice::from_ref(&music), false);
        assert_eq!(title, "No Albums");
        assert!(description.contains("in ~/Music and"), "{description}");

        // Several folders aren't named
        let several = [music, PathBuf::from("/srv/music")];
        let (_, description) = empty_page_text(&several, false);
        assert!(!description.contains('/'), "{description}");

        // A folder outside the home folder keeps its whole path
        let drive = PathBuf::from("/mnt/drive/Music");
        let (title, description) = empty_page_text(std::slice::from_ref(&drive), true);
        assert_eq!(title, "Music Folder Not Available");
        assert!(
            description.starts_with("/mnt/drive/Music is"),
            "{description}"
        );
        let (title, _) = empty_page_text(&several, true);
        assert_eq!(title, "Music Folders Not Available");
    }

    #[test]
    fn favorite_tracks_are_found_by_their_paths() {
        let uris = [
            "file:///m/Bach/02%20Fuga%20%C3%A0%203.flac",
            // A track of a CUE sheet, under the sheet, as the library has it
            "file:///m/Album/Album.cue/track03",
            // Not a file, in a scheme no GIO module takes, so that gvfs
            // can't give it a path of its own
            "x-none://example.org/track.flac",
        ]
        .map(ToString::to_string);
        assert_eq!(
            *favorite_track_paths(&uris),
            [
                PathBuf::from("/m/Bach/02 Fuga à 3.flac"),
                PathBuf::from("/m/Album/Album.cue/track03"),
            ]
        );
    }

    #[test]
    fn favorite_albums_are_followed_to_their_new_ids() {
        let favorite = |id: &str, uri: Option<&str>| play_stats::FavoriteAlbum {
            id: id.to_string(),
            uri: uri.map(ToString::to_string),
        };
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        let favorites = [
            // Shown, with its first track
            favorite("shown", Some("file:///m/shown/01")),
            // Retagged: its first track is in another album now
            favorite("old", Some("file:///m/retagged/01")),
            // Hidden: its first track is still in it
            favorite("hidden", Some("file:///m/hidden/01")),
            // Gone, with its first track
            favorite("gone", Some("file:///m/gone/01")),
            // Marked, but its first track not looked up yet
            favorite("new", None),
        ];
        let shown = HashSet::from(["shown", "new", "retagged"]);
        let album_of_uri = |uri: &str| match uri {
            "file:///m/shown/01" => Some("shown".to_string()),
            "file:///m/retagged/01" => Some("retagged".to_string()),
            "file:///m/hidden/01" => Some("hidden".to_string()),
            _ => None,
        };
        let first_track_uri = |id: &str| (id == "new").then(|| "file:///m/new/01".to_string());

        let updates = favorite_album_updates(&favorites, &shown, album_of_uri, first_track_uri);
        assert_eq!(updates.moves, [pair("old", "retagged")]);
        assert_eq!(updates.uris, [pair("new", "file:///m/new/01")]);
    }
}
