// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

mod covers;
mod pickers;
mod scan;
mod selection;
mod work_row;
mod works;

use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use adw::{prelude::*, subclass::prelude::*};
use covers::*;
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib};
use log::debug;
use pickers::*;
use scan::*;
use std::sync::LazyLock;

use crate::{
    album_cell::AlbumCell,
    audio::{Arrangement, PerformerKind, normalize_search_text},
    cover_image,
    i18n::{i18n, i18n_k, ni18n_f},
    library::{
        self, Album, AlbumSort, Counted, Favorites, ScanEvent, ScanResult, SelectionKey,
        ThumbnailUpdate, TrackFilter,
    },
    play_stats, utils,
    work_sections::{WorkSectionModel, same_section_of},
    zoom::{self, Zoom},
    zoom_row::ZoomRow,
};

// The size of the cover on the copy of an album that follows the pointer
// while it's dragged
const DRAG_COVER_SIZE: i32 = 64;

// The pages of the window that this view shows: the albums of the music
// folders, or their works. They're one widget, which keeps the search, the
// composer and the performer picked, and the Tracks toggle from one page to
// the other
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "DismasLibraryPage")]
pub(crate) enum LibraryPage {
    #[default]
    Albums,
    Works,
}

impl LibraryPage {
    // The name of the window's page
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Albums => "albums",
            Self::Works => "works",
        }
    }

    // The page with the name of a window's page, if it's one of these
    pub(crate) fn named(name: &str) -> Option<Self> {
        [Self::Albums, Self::Works]
            .into_iter()
            .find(|page| page.name() == name)
    }
}

// What the page shows and takes: whole albums; the albums of which only the
// tracks the search matches are taken, with the Tracks toggle on; or the
// works. The title, the heart, Play a Random … and the selection's label
// name it
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryItems {
    Albums,
    Tracks,
    Works,
}

impl LibraryItems {
    // What `page` shows, with the Tracks toggle on or off; the toggle only
    // changes the albums, as the works are taken by their recordings
    pub(crate) fn of(page: LibraryPage, tracks_only: bool) -> Self {
        match (page, tracks_only) {
            (LibraryPage::Albums, false) => Self::Albums,
            (LibraryPage::Albums, true) => Self::Tracks,
            (LibraryPage::Works, _) => Self::Works,
        }
    }
}

// What Play a Random … picks from on the page shown, see
// `LibraryView::random_pool()`: on the Albums page, the albums with a track
// the filter matches, which play whole, or with the Tracks toggle on, the
// tracks the filter matches; on the Works page, the works listed for the
// filter, and the recordings of them that it matches
pub(crate) struct RandomPool {
    pub(crate) items: LibraryItems,
    pub(crate) filter: TrackFilter,
    // Whether the heart is on, which only the Works page leaves out of the
    // filter, as it shows only the favorite works
    pub(crate) favorites_only: bool,
}

// The albums dragged from the grid, with the words of the search each was
// selected in, like the ones the Add button adds, and whether they are
// the selected ones; only Dismas takes them
#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "DismasAlbumDrag")]
pub(crate) struct AlbumDrag {
    pub(crate) albums: Vec<(String, TrackFilter)>,
    pub(crate) selection: bool,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/dachshund7/Dismas/library-view.ui")]
    #[properties(wrapper_type = super::LibraryView)]
    pub(crate) struct LibraryView {
        #[template_child]
        pub(crate) menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(crate) sort_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(crate) select_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub(crate) tracks_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub(crate) favorites_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub(crate) search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(crate) search_problem: TemplateChild<adw::Banner>,
        #[template_child]
        pub(crate) composer_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(crate) composer_popover: TemplateChild<gtk::Popover>,
        #[template_child]
        pub(crate) composer_search: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(crate) composer_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub(crate) performer_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(crate) performer_popover: TemplateChild<gtk::Popover>,
        #[template_child]
        pub(crate) performer_search: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(crate) performer_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub(crate) page_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub(crate) works_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub(crate) albums_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(crate) no_favorites_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(crate) empty_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(crate) empty_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) scan_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub(crate) update_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub(crate) grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub(crate) grid_clamp: TemplateChild<adw::ClampScrollable>,
        #[template_child]
        pub(crate) select_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) selected_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) play_albums_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) add_albums_button: TemplateChild<adw::SplitButton>,

        // Whether clicking an album selects it, instead of showing it
        #[property(get, set = Self::set_selection_mode)]
        pub(crate) selection_mode: Cell<bool>,
        // Whether an album taken from the grid gives only the tracks the
        // search, the composer and the performer picked, and the heart
        // match, rather than the whole album; the Tracks toggle of the
        // header, off at each start
        #[property(get, set = Self::set_tracks_only, explicit_notify)]
        pub(crate) tracks_only: Cell<bool>,
        // The page shown; the works are only shown while a track is part
        // of one
        #[property(get, set = Self::set_page, explicit_notify, builder(LibraryPage::default()))]
        pub(crate) page: Cell<LibraryPage>,
        #[property(get)]
        pub(crate) has_works: Cell<bool>,
        // The section of the main menu with Play a Random …, whose one item
        // names what the page shows
        pub(crate) random_menu: gio::Menu,
        // The step the covers of the grid and of the works are zoomed to,
        // which the cells follow
        #[property(get)]
        pub(crate) cover_zoom: Cell<i32>,
        // The works the search finds, and the filter and the generation of
        // the library they were read for, `None` until they're read. The
        // generation goes up each time the library is read again, as the
        // tracks or the hidden folders can have changed
        pub(crate) works: RefCell<Option<gio::ListStore>>,
        pub(crate) works_filter: RefCell<Option<(TrackFilter, u32)>>,
        pub(crate) works_generation: Cell<u32>,
        // The work of the list that's open, showing the covers of its
        // recordings, and the rows the list shows its works in, by their keys,
        // to open and close them. The generation goes up each time the
        // covers of a work are read, so that those of one opened before
        // are dropped
        pub(crate) expanded_work: RefCell<Option<super::works::ExpandedWork>>,
        pub(crate) work_rows: RefCell<HashMap<String, glib::WeakRef<work_row::WorkRow>>>,
        pub(crate) covers_generation: Cell<u32>,
        // The tick callback that puts the line of the work opening in its
        // place, see `show_when_open()`
        pub(crate) placing: RefCell<Option<gtk::TickCallbackId>>,
        // The glide that brings a cover pointed out into view, see
        // `point_out_cover()`, stopped when another work opens
        pub(crate) cover_glide: RefCell<Option<adw::TimedAnimation>>,
        // The work the user is opening, while its covers are read, see
        // `open_work()`
        pub(crate) opening: RefCell<Option<super::works::UserOpening>>,
        // A work to go to once the works are read, and the number of works
        // gone to, so that a new search's list isn't scrolled to its top
        // once one was
        pub(crate) pending_work: RefCell<Option<super::works::PendingWork>>,
        pub(crate) works_revealed: Cell<u32>,
        // The selected albums, or on the Works page the covers of the open
        // work's recordings, see `Album::selection_key()`; a new search
        // clears them, so they're all among the albums it shows
        pub(crate) selected: RefCell<HashSet<SelectionKey>>,
        // The ids of the albums marked as favorites
        pub(crate) favorite_albums: RefCell<BTreeSet<String>>,
        // The paths of the favorite tracks, read when the heart first needs
        // them, see `favorite_tracks()`
        pub(crate) favorite_tracks: RefCell<Option<Arc<[PathBuf]>>>,
        // Whether the focus of the window is followed, see
        // `setup_lost_focus()`
        pub(crate) follows_focus: Cell<bool>,
        // The last album selected or unselected with a click, where
        // Shift+click starts selecting from
        pub(crate) selection_anchor: glib::WeakRef<Album>,
        // Tracks are being added to the queue
        pub(crate) loading: Cell<bool>,
        // The drag source of the album being dragged, to cancel the drag
        pub(crate) drag_source: RefCell<Option<gtk::DragSource>>,

        pub(crate) model: RefCell<Option<gio::ListStore>>,
        pub(crate) filter: RefCell<Option<gtk::CustomFilter>>,
        pub(crate) sorter: RefCell<Option<gtk::CustomSorter>>,
        pub(crate) settings: OnceCell<gio::Settings>,
        // The albums matching the search, if there is one, with the
        // number of their tracks that match it and the number of all of them
        pub(crate) search_matches: RefCell<Option<HashMap<String, (u32, u32)>>>,
        // The words of the search, and the composer and the performer, that
        // the matches are for
        pub(crate) search_filter: RefCell<TrackFilter>,
        // The composer and the performer whose albums the grid shows,
        // picked from the lists next to the search
        pub(crate) composer: RefCell<Option<library::ComposerInfo>>,
        pub(crate) performer: RefCell<Option<library::PerformerInfo>>,
        pub(crate) textures: RefCell<TextureCache>,
        pub(crate) pending_covers: RefCell<super::PendingCovers>,
        // Bumped when the size the covers are read at changes, so that the
        // covers still to be read at the size before aren't read, and those
        // being read aren't kept; the reading threads look at it too
        pub(crate) cover_generation: Arc<AtomicU32>,
        // The size the last scan made the covers at, 0 before the first
        pub(crate) thumbnail_size: Cell<u32>,
        // The buttons that zoom the covers, in the main menu
        pub(crate) zoom_row: ZoomRow,
        // The `library` actions, of which zooming is only on short of the
        // ends it goes to
        pub(crate) actions: gio::SimpleActionGroup,
        // The cells of the grid that are mapped, which are about the ones
        // on screen
        pub(crate) mapped_cells: Cell<u32>,
        // The music folders are being read
        #[property(get)]
        pub(crate) scanning: Cell<bool>,
        // The folders changed while they were being read, so they're read
        // again once it's done; two scans would write at once
        pub(crate) rescan_pending: Cell<bool>,
        // Watches the folders of the library, to read them again when
        // their files change
        pub(crate) watch: OnceCell<Rc<library::FolderWatch>>,
        // A music folder that's there can't be watched, so it's read again
        // when the albums are shown, if the last scan finished a while ago
        pub(crate) unwatched: Cell<bool>,
        pub(crate) last_scan_finished: Cell<Option<Instant>>,
        // Bumped for each reload of the albums
        pub(crate) reload_generation: Cell<u32>,
        // The music folders that aren't there, as of the last reload, whose
        // albums are hidden
        pub(crate) hidden_folders: RefCell<Vec<PathBuf>>,
        // No album is shown, and the music folders that are missing or
        // empty have albums, as when their drive isn't plugged in
        pub(crate) albums_unavailable: Cell<bool>,
        // The albums the grid shows, as they were read
        pub(crate) shown_albums: RefCell<Vec<library::AlbumInfo>>,
        // A scan made covers again since the albums were last shown, which
        // can keep their paths, and whether it changed tracks, which a
        // search can find or not find now
        pub(crate) covers_changed: Cell<bool>,
        pub(crate) tracks_changed: Cell<bool>,
        // What the albums are sorted by, read from the settings once for
        // all the comparisons of a sort
        pub(crate) sort_order: Cell<Option<(AlbumSort, bool)>>,
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
            // The grid selects albums, and the Works page among the covers
            // of a work
            self.obj().set_selection_mode(false);
            self.items_changed();
            self.obj().notify_page();
        }

        // The same albums are shown either way, so the selection is kept,
        // and its label counts what's taken from them
        fn set_tracks_only(&self, tracks_only: bool) {
            if self.tracks_only.replace(tracks_only) == tracks_only {
                return;
            }
            self.items_changed();
            self.obj().notify_tracks_only();
        }

        // The page or the toggle changed what's shown (`items()`)
        fn items_changed(&self) {
            let obj = self.obj();
            obj.update_view();
            // The heart beside the search shows the favorites of what's
            // shown: the favorite albums, the albums with favorite tracks
            // with the toggle on, or the favorite works, so the albums it
            // shows are searched for again
            if self.favorites_button.is_active() {
                obj.update_album_matches();
            }
            obj.update_random_item();
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
            let obj = self.obj();
            obj.update_random_item();
            crate::utils::setup_primary_menu_button(
                &self.menu_button,
                Some(&self.zoom_row),
                Some(self.random_menu.upcast_ref()),
            );

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
                    // The URIs of the tracks to read again, as their files or
                    // cover files changed since the last scan, and of those
                    // to read again if they have no cover, see
                    // `ScanResult`; the number of tracks that are new,
                    // changed or gone, and whether the user asked for the
                    // scan
                    Signal::builder("scan-finished")
                        .param_types([
                            Vec::<String>::static_type(),
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
    pub(crate) struct LibraryView(ObjectSubclass<imp::LibraryView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for LibraryView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl LibraryView {
    pub(crate) fn connect_album_activated<
        F: Fn(&Self, &Album, &TrackFilter, &PickNames) + 'static,
    >(
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

    pub(crate) fn connect_work_menu_requested<
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

    pub(crate) fn connect_album_menu_requested<F: Fn(&Self, &AlbumCell, f64, f64) + 'static>(
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

    pub(crate) fn connect_add_albums<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "add-albums",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub(crate) fn connect_play_albums<F: Fn(&Self) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "play-albums",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub(crate) fn connect_albums_drag_begin<F: Fn(&Self) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "albums-drag-begin",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub(crate) fn connect_albums_drag_end<F: Fn(&Self) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "albums-drag-end",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub(crate) fn connect_scan_finished<
        F: Fn(&Self, Vec<String>, Vec<String>, u32, bool) + 'static,
    >(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "scan-finished",
            false,
            glib::closure_local!(move |view: &Self,
                                       uris: Vec<String>,
                                       uris_without_cover: Vec<String>,
                                       n_changed: u32,
                                       requested: bool| {
                f(view, uris, uris_without_cover, n_changed, requested);
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
                // say how many of their tracks it does
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
    pub(crate) fn cancel_drag(&self) -> bool {
        let source = self.imp().drag_source.borrow().clone();
        source.is_some_and(|source| utils::cancel_drag(&source))
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
    pub(crate) fn hidden_folders(&self) -> Vec<PathBuf> {
        self.imp().hidden_folders.borrow().clone()
    }

    // Whether the page shows the works, rather than the albums
    pub(crate) fn shows_works(&self) -> bool {
        self.imp().page.get() == LibraryPage::Works
    }

    // What the page shows and takes, with the Tracks toggle
    pub(crate) fn items(&self) -> LibraryItems {
        LibraryItems::of(self.page(), self.tracks_only())
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

    // The works take the place of the grid, without its sorting and its
    // Tracks toggle, and its title; with the toggle on, the title and the
    // search name the tracks, as only those the search matches are taken
    fn update_view(&self) {
        let imp = self.imp();
        let works = self.shows_works();
        let (title, search) = match self.items() {
            LibraryItems::Albums => (i18n("Albums"), i18n("Search albums")),
            LibraryItems::Tracks => (i18n("Tracks"), i18n("Search tracks")),
            LibraryItems::Works => (i18n("Works"), i18n("Search works")),
        };
        imp.page_title.set_title(&title);
        imp.search_entry.set_placeholder_text(Some(&search));
        imp.search_entry
            .update_property(&[gtk::accessible::Property::Label(&search)]);
        imp.sort_button.set_visible(!works);
        imp.tracks_button.set_visible(!works);
        // What a click does, as the heart over an album's tracks says
        imp.tracks_button
            .set_tooltip_text(Some(&if self.tracks_only() {
                i18n("Take Whole Albums")
            } else {
                i18n("Take Only the Matching Tracks")
            }));
        // It names what it shows, for screen readers too
        let favorites = match self.items() {
            LibraryItems::Albums => i18n("Show Only Favorite Albums"),
            LibraryItems::Tracks => i18n("Show Only Favorite Tracks"),
            LibraryItems::Works => i18n("Show Only Favorite Works"),
        };
        imp.favorites_button.set_tooltip_text(Some(&favorites));
        // So does the select button: the covers of a work are its recordings
        imp.select_button.set_tooltip_text(Some(&if works {
            i18n("Select Recordings")
        } else {
            i18n("Select Albums")
        }));
        self.update_match_counts();
        self.update_select_button();
        self.update_selection();
        self.update_stack();
        self.update_works();
    }

    // The heart beside the search shows only the favorites: the albums on
    // the Albums page, or the albums with favorite tracks with the Tracks
    // toggle on, and the works on the Works page. It's filled and highlighted while it
    // does
    fn setup_favorites_filter(&self) {
        let button = &self.imp().favorites_button;
        // Its tooltip says what it shows, see `update_view()`
        utils::setup_heart_button(button, None);
        button.connect_toggled(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                // The works are read again, as for a favorite marked
                this.refresh_favorites();
                this.update_album_matches();
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
        let favorites = if self.heart_filters(LibraryItems::Albums) {
            Some(Favorites::Albums(
                imp.favorite_albums.borrow().iter().cloned().collect(),
            ))
        } else if self.heart_filters(LibraryItems::Tracks) {
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
            tracks: None,
            favorites,
        }
    }

    // Whether the heart shows only the favorite `items`, which are the
    // ones shown
    fn heart_filters(&self, items: LibraryItems) -> bool {
        self.imp().favorites_button.is_active() && self.items() == items
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
    // with the player's heart: the albums the heart shows with the Tracks
    // toggle on change. An album's tracks open in the pane stay as they were,
    // as they do for a new search
    pub(crate) fn favorite_tracks_changed(&self) {
        self.imp().favorite_tracks.replace(None);
        // A search for the favorites finds them, or leaves them out
        let searched = TrackFilter::query(&self.imp().search_entry.text())
            .is_some_and(|query| query.needs().favorites);
        if searched {
            self.update_search();
        } else if self.heart_filters(LibraryItems::Tracks) {
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
    // of their tracks it does, whether the Tracks toggle takes only those
    // or not, as it's the same albums either way
    fn update_match_counts(&self) {
        let imp = self.imp();
        let matches = imp.search_matches.borrow();
        let matches = matches.as_ref();
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
    // performer it picks: with the Tracks toggle on, the words of the
    // search and the composer and the performer picked; off, nothing, as
    // the albums are taken whole. The covers of the Works page take the tracks
    // of their recordings, see `taken_by()`, and the grid isn't there
    pub(crate) fn taken(&self) -> (TrackFilter, PickNames) {
        match self.items() {
            LibraryItems::Albums | LibraryItems::Works => Default::default(),
            LibraryItems::Tracks => (self.search_filter(), self.search_pick_names()),
        }
    }

    // What Play a Random … picks from: what the search, the composer and
    // the performer picked, and the heart ask for on the page shown, as
    // the grid and the works are searched with it, rather than what the
    // page shows, which is still the other page's just after a page switch
    pub(crate) fn random_pool(&self) -> RandomPool {
        RandomPool {
            items: self.items(),
            filter: self.wanted_filter(),
            favorites_only: self.imp().favorites_button.is_active(),
        }
    }

    // Names what Play a Random … plays on the page shown
    fn update_random_item(&self) {
        let label = match self.items() {
            LibraryItems::Albums => i18n("Play a _Random Album"),
            LibraryItems::Tracks => i18n("Play a _Random Track"),
            LibraryItems::Works => i18n("Play a _Random Work"),
        };
        let menu = &self.imp().random_menu;
        menu.remove_all();
        menu.append(Some(&label), Some("win.play-random"));
    }

    // What's taken from the album of a cell: among the covers of the
    // recordings of a work, its recording's tracks, and otherwise as for
    // the grid, see `taken()`
    pub(crate) fn taken_by(&self, cell: &AlbumCell) -> (TrackFilter, PickNames) {
        match cell.work() {
            Some(work) => works::taken_from_recording(work.paths, work.name),
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
    pub(crate) fn album(&self, album_id: &str) -> Option<Album> {
        self.imp()
            .model
            .borrow()
            .as_ref()?
            .iter::<Album>()
            .flatten()
            .find(|a| a.id_str() == album_id)
    }

    // Whether an album is marked as a favorite
    pub(crate) fn is_favorite_album(&self, album_id: &str) -> bool {
        self.imp().favorite_albums.borrow().contains(album_id)
    }

    // Follows albums marked as favorites or unmarked: the hearts of their
    // covers, and for those marked, the first track, which finds an album
    // again once a change to its tags gives it another id. That's looked up
    // in the library in another thread, so that marking doesn't wait for it
    pub(crate) fn favorite_albums_changed(&self, ids: &[String], favorite: bool) {
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
        if self.heart_filters(LibraryItems::Albums) {
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
    pub(crate) fn reveal_album(&self, album_id: &str) -> Option<Album> {
        let imp = self.imp();
        let album = self.album(album_id)?;

        // The heart hides the albums that aren't favorites, whose matches
        // can still be read, as when it's gone to from the Works page. With
        // the Tracks toggle on, the matches are those of the favorite
        // tracks, so a favorite album without one clears the heart, though
        // the albums taken whole would show it
        let hidden = imp
            .search_matches
            .borrow()
            .as_ref()
            .is_some_and(|m| !m.contains_key(album_id))
            || (self.heart_filters(LibraryItems::Albums) && !self.is_favorite_album(album_id));
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
    pub(crate) fn open_album(&self, album_id: &str) {
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
    pub(crate) fn focus_search(&self) {
        let imp = self.imp();
        if imp.albums_stack.visible_child_name().as_deref() == Some("empty") {
            imp.empty_button.grab_focus();
        } else {
            imp.search_entry.grab_focus();
        }
    }

    pub(crate) fn search_entry(&self) -> gtk::SearchEntry {
        self.imp().search_entry.get()
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
    // of the search matches. Just after the page switches or the Tracks
    // toggle flips, the matches are still those of the other's favorites
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
        let (title, description) = match (self.items(), searched) {
            (LibraryItems::Albums, false) => (
                i18n("No Favorite Albums"),
                i18n(
                    "Mark an album as a favorite from its menu, or with the heart over its tracks",
                ),
            ),
            (LibraryItems::Tracks, false) => (
                i18n("No Favorite Tracks"),
                i18n(
                    "Mark a track as a favorite from its menu, or with the heart next to the sleep timer",
                ),
            ),
            (LibraryItems::Works, false) => (
                i18n("No Favorite Works"),
                i18n("Mark a work or one of its recordings as a favorite from its menu"),
            ),
            (_, true) => (
                i18n("No Results Found"),
                match self.items() {
                    LibraryItems::Albums => i18n("None of the favorite albums match the search"),
                    LibraryItems::Tracks => i18n("None of the favorite tracks match the search"),
                    LibraryItems::Works => i18n("None of the favorite works match the search"),
                },
            ),
        };
        imp.no_favorites_page.set_title(&title);
        imp.no_favorites_page.set_description(Some(&description));
    }
}

// The names of the composer and the performer whose albums are shown, if
// any, or of the work whose tracks are taken from an album, which the bar
// over an album's tracks says
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PickNames {
    pub(crate) composer: Option<String>,
    pub(crate) performer: Option<String>,
    pub(crate) work: Option<String>,
}

// The paths of the favorite tracks, from their URIs, as the library keeps
// them; a track of a CUE sheet has its path under the sheet in both
fn favorite_track_paths(uris: &[String]) -> Arc<[PathBuf]> {
    uris.iter()
        .filter_map(|uri| gio::File::for_uri(uri).path())
        .collect()
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
    fn the_toggle_takes_only_tracks_from_the_albums() {
        assert_eq!(
            LibraryItems::of(LibraryPage::Albums, false),
            LibraryItems::Albums
        );
        assert_eq!(
            LibraryItems::of(LibraryPage::Albums, true),
            LibraryItems::Tracks
        );
        // The works are taken by their recordings either way
        assert_eq!(
            LibraryItems::of(LibraryPage::Works, true),
            LibraryItems::Works
        );
        // The Tracks page is gone
        assert_eq!(LibraryPage::named("tracks"), None);
        assert_eq!(LibraryPage::named("works"), Some(LibraryPage::Works));
    }

    #[test]
    fn each_zoom_step_changes_the_size_of_the_covers() {
        // A step starts where the one before ends, so the covers change
        // size even where the grid keeps its columns
        for sizes in [&COVER_SIZES, &works::WORK_COVER_SIZES] {
            assert_eq!(sizes.len(), (COVER_ZOOM.max - COVER_ZOOM.min + 1) as usize);
            for pair in sizes.windows(2) {
                let ((min, max), (next_min, _)) = (pair[0], pair[1]);
                assert!(min < max && max <= next_min, "{sizes:?}");
            }
        }
        assert_eq!(grid_cover_sizes(0), (160, 240));
        assert_eq!(grid_cover_sizes(COVER_ZOOM.min), (90, 120));
        assert_eq!(grid_cover_sizes(COVER_ZOOM.max), (240, 320));
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
