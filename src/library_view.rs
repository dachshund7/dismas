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
    i18n::{i18n, i18n_k, ni18n_f},
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

// The number of covers kept in memory, enough for a few screenfuls of
// the grid; keeping all of them would take hundreds of megabytes
const MAX_CACHED_COVERS: usize = 100;

// The pictures waiting for each cover to load, as album IDs and pictures,
// keyed by the path of the cover
type PendingCovers = HashMap<String, Vec<(String, glib::WeakRef<gtk::Picture>)>>;

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
        // The selected albums, with the words of the search each was
        // selected in, so that only the tracks that matched it are added
        pub selected: RefCell<HashMap<String, Vec<String>>>,
        // The last album selected or unselected with a click, where
        // Shift+click starts selecting from
        pub selection_anchor: glib::WeakRef<Album>,
        // Tracks are being added to the queue
        pub loading: Cell<bool>,

        pub model: RefCell<Option<gio::ListStore>>,
        pub filter: RefCell<Option<gtk::CustomFilter>>,
        pub sorter: RefCell<Option<gtk::CustomSorter>>,
        pub settings: OnceCell<gio::Settings>,
        // The albums matching the search, if there is one
        pub search_matches: RefCell<Option<HashSet<String>>>,
        // The words of the search the matches are for
        pub search_words: RefCell<Vec<String>>,
        pub textures: RefCell<TextureCache>,
        pub pending_covers: RefCell<super::PendingCovers>,
        // The Music folder is being read
        #[property(get)]
        pub scanning: Cell<bool>,
        // Bumped for each reload of the albums
        pub reload_generation: Cell<u32>,
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
                    Some(matches) => matches.contains(&album.id()),
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
                let (sort, descending) = this.sort_order();
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
                let cover = SquareBin::new(&overlay, MIN_COVER_SIZE, MAX_COVER_SIZE);

                let title = gtk::Label::builder()
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(1)
                    .hexpand(true)
                    .css_classes(["album-title"])
                    .has_tooltip(true)
                    .build();
                // The title is often ellipsized, so the tooltip shows all of it;
                // setting the tooltip text on each bind is slow, as it also updates
                // the accessible description, so we only fill it in when it's shown
                title.connect_query_tooltip(|label, _, _, _, tooltip| {
                    tooltip.set_text(Some(&label.label()));
                    true
                });
                let artist = gtk::Label::builder()
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(1)
                    .hexpand(true)
                    .css_classes(["caption", "dim-label"])
                    .build();

                let content = AlbumCell::default();
                content.set_orientation(gtk::Orientation::Vertical);
                content.set_spacing(3);
                content.add_css_class("album-cell");
                content.append(&cover);
                content.append(&title);
                content.append(&artist);

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
                let content = item.child().unwrap();
                let cover = content.first_child().unwrap();
                let picture = cover
                    .first_child()
                    .and_then(|overlay| overlay.last_child())
                    .and_downcast::<gtk::Picture>()
                    .unwrap();
                let title = cover.next_sibling().and_downcast::<gtk::Label>().unwrap();
                let artist = content.last_child().and_downcast::<gtk::Label>().unwrap();

                title.set_label(&album.title());
                artist.set_label(&album.artist());

                picture.set_paintable(gdk::Paintable::NONE);
                this.load_cover(&album, &picture);
            }
        ));

        factory.upcast()
    }

    // Sets the cover of the album on the picture, once it has been loaded
    // in a separate thread; grid cells are recycled when scrolling, so the
    // picture might be showing another album by then
    fn load_cover(&self, album: &Album, picture: &gtk::Picture) {
        let imp = self.imp();
        picture.set_widget_name(&album.id());

        let Some(path) = album.cover_path() else {
            return;
        };

        if let Some(texture) = imp.textures.borrow_mut().get(&path) {
            picture.set_paintable(Some(&texture));
            return;
        }

        // Another picture is already waiting for the same cover
        let mut pending = imp.pending_covers.borrow_mut();
        if let Some(waiting) = pending.get_mut(&path) {
            waiting.push((album.id(), picture.downgrade()));
            return;
        }
        pending.insert(path.clone(), vec![(album.id(), picture.downgrade())]);

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

                for (id, picture) in waiting.into_iter().flatten() {
                    if let Some(picture) = picture.upgrade()
                        && picture.widget_name() == id
                    {
                        picture.set_paintable(Some(&texture));
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

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok((words, matches)) = gio::spawn_blocking(move || {
                    let matches = library::search_albums(&words);
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

    fn set_search_matches(&self, words: Vec<String>, matches: Option<HashSet<String>>) {
        self.imp().search_words.replace(words);
        self.imp().search_matches.replace(matches);
        if let Some(filter) = self.imp().filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
    }

    // The normalized words of the search the grid is filtered by, if any
    pub fn search_words(&self) -> Vec<String> {
        self.imp().search_words.borrow().clone()
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
            .is_some_and(|m| !m.contains(album_id));
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
        let mut map = self.imp().selected.borrow_mut();
        if selected {
            map.insert(album.id(), self.search_words());
        } else {
            map.remove(&album.id());
        }
        drop(map);
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

    // The selected albums, in the order of the grid, with the words of
    // the search each was selected in
    pub fn selected_albums(&self) -> Vec<(String, Vec<String>)> {
        let imp = self.imp();
        let selected = imp.selected.borrow();
        let Some(model) = imp.model.borrow().clone() else {
            return Vec::new();
        };
        model
            .iter::<Album>()
            .flatten()
            .filter_map(|album| {
                let id = album.id();
                selected.get(&id).map(|words| (id, words.clone()))
            })
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
    // the one before it, if it takes longer
    pub fn reload(&self) {
        let imp = self.imp();
        let generation = imp.reload_generation.get().wrapping_add(1);
        imp.reload_generation.set(generation);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let albums = gio::spawn_blocking(library::albums)
                    .await
                    .unwrap_or_default();
                if this.imp().reload_generation.get() == generation {
                    this.show_albums(albums);
                }
            }
        ));
    }

    fn show_albums(&self, albums: Vec<library::AlbumInfo>) {
        let imp = self.imp();
        let albums: Vec<Album> = albums.into_iter().map(Album::new).collect();
        debug!("Library: {} albums", albums.len());

        // Keep the selection, minus the albums that are gone
        {
            let mut selected = imp.selected.borrow_mut();
            let ids: HashSet<String> = albums.iter().map(Album::id).collect();
            selected.retain(|id, _| ids.contains(id));
            for album in &albums {
                album.set_selected(selected.contains_key(&album.id()));
            }
        }
        self.update_selection();

        if let Some(model) = imp.model.borrow().as_ref() {
            model.splice(0, model.n_items(), &albums);
        }

        imp.textures.borrow_mut().clear();
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

    // Shows the albums we know about, then looks for changes in the Music
    // folder in a separate thread; `requested` is whether the user asked
    // for it, to tell them what it found
    pub fn start_scan(&self, requested: bool) {
        let imp = self.imp();
        if imp.scanning.get() {
            return;
        }
        self.set_scanning(true);

        let Some(music_dir) = library::music_dir() else {
            self.set_scanning(false);
            self.reload();
            return;
        };

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
        std::thread::spawn(move || library::scan(&music_dir, &sender));

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

                // The channel also closes if the scan panicked
                this.set_scanning(false);
                this.reload();

                let uris: Vec<String> = result
                    .updated
                    .iter()
                    .map(|path| gio::File::for_path(path).uri().to_string())
                    .collect();
                let n_changed = u32::try_from(result.n_changed).unwrap_or(u32::MAX);
                this.emit_by_name::<()>("scan-finished", &[&uris, &n_changed, &requested]);
            }
        ));
    }
}
