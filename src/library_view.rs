// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib};
use log::debug;
use once_cell::sync::Lazy;

use crate::{
    audio::normalize_search_text,
    i18n::{i18n, i18n_k, ni18n_f},
    library::{self, Album, ScanEvent},
};

// The size of the covers in the grid
const CELL_COVER_SIZE: i32 = 160;

// The number of covers kept in memory, enough for a few screenfuls of
// the grid; keeping all of them would take hundreds of megabytes
const MAX_CACHED_COVERS: usize = 300;

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
    #[template(resource = "/io/bassi/Amberol/library-view.ui")]
    #[properties(wrapper_type = super::LibraryView)]
    pub struct LibraryView {
        #[template_child]
        pub back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rescan_button: TemplateChild<gtk::Button>,
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
        // Tracks are being added to the playlist
        pub loading: Cell<bool>,

        pub model: RefCell<Option<gio::ListStore>>,
        pub filter: RefCell<Option<gtk::CustomFilter>>,
        // The albums matching the search, if there is one
        pub search_matches: RefCell<Option<HashSet<String>>>,
        // The words of the search the matches are for
        pub search_words: RefCell<Vec<String>>,
        pub textures: RefCell<TextureCache>,
        pub pending_covers: RefCell<HashMap<String, Vec<(String, glib::WeakRef<gtk::Picture>)>>>,
        pub scanning: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LibraryView {
        const NAME: &'static str = "AmberolLibraryView";
        type Type = super::LibraryView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
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

            let obj = self.obj();
            obj.setup_grid();
            obj.connect_signals();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: Lazy<Vec<Signal>> = Lazy::new(|| {
                vec![
                    Signal::builder("album-activated")
                        .param_types([Album::static_type()])
                        .build(),
                    // Add or play the tracks of the selected albums
                    Signal::builder("add-albums").build(),
                    Signal::builder("play-albums").build(),
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

        let filter_model = gtk::FilterListModel::new(Some(model.clone()), Some(filter.clone()));
        let selection = gtk::NoSelection::new(Some(filter_model));

        imp.grid_view.set_model(Some(&selection));
        imp.grid_view.set_factory(Some(&self.album_factory()));

        imp.model.replace(Some(model));
        imp.filter.replace(Some(filter));
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
                let cover = gtk::Overlay::builder()
                    .width_request(CELL_COVER_SIZE)
                    .height_request(CELL_COVER_SIZE)
                    .halign(gtk::Align::Center)
                    .overflow(gtk::Overflow::Hidden)
                    .css_classes(["album-cover"])
                    .child(&placeholder)
                    .build();
                cover.add_overlay(&picture);

                let check = gtk::CheckButton::builder()
                    .halign(gtk::Align::Start)
                    .valign(gtk::Align::Start)
                    .css_classes(["selection-mode"])
                    .build();
                this.bind_property("selection-mode", &check, "visible")
                    .sync_create()
                    .build();
                item.property_expression("item")
                    .chain_property::<Album>("selected")
                    .bind(&check, "active", gtk::Widget::NONE);
                check.connect_toggled(clone!(
                    #[weak]
                    this,
                    #[weak]
                    item,
                    move |check| {
                        let Some(album) = item.item().and_downcast::<Album>() else {
                            return;
                        };
                        // Also called when the cell shows another album
                        if album.selected() != check.is_active() {
                            this.set_album_selected(&album, check.is_active());
                        }
                    }
                ));
                cover.add_overlay(&check);

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

                let content = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(3)
                    .width_request(CELL_COVER_SIZE)
                    .build();
                content.append(&cover);
                content.append(&title);
                content.append(&artist);

                // Ctrl+click selects the album, like in the playlist
                let click = gtk::GestureClick::new();
                click.connect_pressed(clone!(
                    #[weak]
                    this,
                    #[weak]
                    item,
                    move |gesture, _, _, _| {
                        if !gesture
                            .current_event_state()
                            .contains(gdk::ModifierType::CONTROL_MASK)
                        {
                            return;
                        }
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        if let Some(album) = item.item().and_downcast::<Album>() {
                            this.toggle_album(&album);
                        }
                    }
                ));
                content.add_controller(click);

                item.set_child(Some(&content));
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
                    .last_child()
                    .and_then(|check| check.prev_sibling())
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
                    if let Some(picture) = picture.upgrade() {
                        if picture.widget_name() == id {
                            picture.set_paintable(Some(&texture));
                        }
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

        imp.rescan_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.start_scan()
        ));
    }

    // Looks for the albums with a track that contains every word of the
    // search, in any of its tags, like searching the playlist
    fn update_search(&self) {
        let text = self.imp().search_entry.text();
        let words: Vec<String> = normalize_search_text(&text)
            .split_whitespace()
            .map(|w| w.to_string())
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

    // Scrolls the grid to an album, clearing the search if it hides it
    pub fn reveal_album(&self, album_id: &str) -> Option<Album> {
        let imp = self.imp();
        let album = imp
            .model
            .borrow()
            .as_ref()?
            .iter::<Album>()
            .flatten()
            .find(|a| a.id() == album_id)?;

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
        if !self.selection_mode() {
            self.set_selection_mode(true);
            self.set_album_selected(album, true);
        } else {
            self.set_album_selected(album, !album.selected());
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

    // Albums can't be added while tracks are being added to the playlist
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

    // Whether there is a player to go back to
    pub fn set_can_go_back(&self, can_go_back: bool) {
        self.imp().back_button.set_visible(can_go_back);
    }

    // Reads the albums from the database
    pub fn reload(&self) {
        let imp = self.imp();
        let albums: Vec<Album> = library::albums().into_iter().map(Album::new).collect();
        debug!("Library: {} albums", albums.len());

        // Keep the selection, minus the albums that are gone
        {
            let mut selected = imp.selected.borrow_mut();
            let ids: HashSet<String> = albums.iter().map(|a| a.id()).collect();
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
        let n_albums = imp.model.borrow().as_ref().map_or(0, |m| m.n_items());
        let page = if n_albums > 0 {
            "albums"
        } else if imp.scanning.get() {
            "scanning"
        } else {
            "empty"
        };
        imp.albums_stack.set_visible_child_name(page);
    }

    // Shows the albums we know about, then looks for changes in the Music
    // folder in a separate thread
    pub fn start_scan(&self) {
        let imp = self.imp();
        if imp.scanning.replace(true) {
            return;
        }

        let Some(music_dir) = library::music_dir() else {
            imp.scanning.set(false);
            self.reload();
            return;
        };

        self.reload();
        imp.rescan_button.set_sensitive(false);
        imp.scan_progress.set_fraction(0.0);
        imp.scan_progress.set_text(None);

        let (sender, receiver) = async_channel::unbounded();
        std::thread::spawn(move || library::scan(&music_dir, &sender));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                while let Ok(event) = receiver.recv().await {
                    let imp = this.imp();
                    match event {
                        ScanEvent::Progress(done, total) => {
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

                            // When the grid is already showing, a thin bar
                            // at the bottom is enough
                            imp.update_progress.set_fraction(fraction);
                            imp.update_progress.set_visible(
                                imp.albums_stack.visible_child_name().as_deref() == Some("albums"),
                            );
                        }
                        ScanEvent::Finished => {
                            imp.scanning.set(false);
                            imp.update_progress.set_visible(false);
                            imp.rescan_button.set_sensitive(true);
                            this.reload();
                            break;
                        }
                    }
                }
            }
        ));
    }
}
