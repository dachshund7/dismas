// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod albums;
mod bindings;
mod loading;
mod now_playing;
mod playlist;
mod selected_playlist;
mod selection;
mod sidebar;
mod track_menu;

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    str::FromStr,
    time::Instant,
};

use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, gdk, gio, glib, graphene, prelude::*};
use log::debug;

use crate::{
    album_tracks::AlbumTracks,
    audio::{AudioPlayer, CoverCache, RepeatMode, ReplayGainMode, Track, load_tracks},
    config::APPLICATION_ID,
    drag_overlay::DragOverlay,
    i18n::{i18n, i18n_k, ni18n_f, ni18n_k},
    library::{self, Album},
    library_view::LibraryView,
    page_switcher::PageSwitcher,
    playback_control::PlaybackControl,
    playlist_view::PlaylistView,
    playlists::{self, NameError},
    playlists_page::PlaylistsPage,
    queue_row::QueueRow,
    search::SearchFilter,
    track_cover::TrackCover,
    track_details::TrackDetails,
    track_info_dialog::TrackInfoDialog,
    utils,
    waveform_view::WaveformView,
    work_sections::{WorkSectionModel, work_header_factory},
};

// What happens once tracks are added to the queue
#[derive(Clone, Copy, PartialEq)]
enum QueueMode {
    // Stay in the album grid, to keep browsing
    Add,
    // Show the player, e.g. after opening files
    Show,
    // Show the player, without announcing the tracks, for the queue
    // of the last session
    Restore,
    // Show the player, and play the tracks from the given one
    Play(u32),
}

// Where the menu of a track is shown
#[derive(Clone, Copy)]
enum TrackMenu {
    Queue,
    // The tracks of an album from the grid
    Album,
}

mod imp {
    use super::*;

    #[derive(CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::Window)]
    #[template(resource = "/io/bassi/Amberol/window.ui")]
    pub struct Window {
        // Template widgets
        #[template_child]
        pub drag_overlay: TemplateChild<DragOverlay>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub track_cover: TemplateChild<TrackCover>,
        #[template_child]
        pub track_details: TemplateChild<TrackDetails>,
        #[template_child]
        pub waveform_view: TemplateChild<WaveformView>,
        #[template_child]
        pub elapsed_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub remaining_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub playback_control: TemplateChild<PlaybackControl>,
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub playlist_view: TemplateChild<PlaylistView>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub library_view: TemplateChild<LibraryView>,
        #[template_child]
        pub playlists_page: TemplateChild<PlaylistsPage>,
        #[template_child]
        pub sidebar_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub album_tracks: TemplateChild<AlbumTracks>,
        #[template_child]
        pub sidebar_handle: TemplateChild<gtk::Box>,

        pub provider: gtk::CssProvider,
        pub settings: gio::Settings,

        #[property(get, set = Self::set_queue_shuffled, explicit_notify)]
        pub queue_shuffled: Cell<bool>,
        #[property(get, set = Self::set_playlist_visible, explicit_notify)]
        pub playlist_visible: Cell<bool>,
        #[property(get, set = Self::set_queue_selection, explicit_notify)]
        pub queue_selection: Cell<bool>,
        // The last track selected or unselected with a click, where
        // Shift+click starts selecting from
        pub queue_anchor: glib::WeakRef<Track>,
        #[property(get, set = Self::set_queue_search, explicit_notify)]
        pub queue_search: Cell<bool>,
        #[property(get, set = Self::set_replaygain_mode, explicit_notify, builder(ReplayGainMode::default()))]
        pub replaygain_mode: Cell<ReplayGainMode>,
        pub sidebar_width: Cell<i32>,
        #[property(get, set = Self::set_library_visible, explicit_notify)]
        pub library_visible: Cell<bool>,
        // The list of playlists, shown over the player or the album grid
        #[property(get, set = Self::set_playlists_visible, explicit_notify)]
        pub playlists_visible: Cell<bool>,
        // The visible page: "player", "library" or "playlists"
        #[property(name = "page", get = Self::page, set = Self::set_page, explicit_notify, type = String)]
        page: std::marker::PhantomData<String>,
        // Whether there is anything to play, to go to the player
        #[property(get)]
        pub player_available: Cell<bool>,
        // Whether the queue might still be restored or opened at startup
        pub starting: Cell<bool>,
        // Whether to hide the focus again once the page is shown
        pub hide_focus_after_transition: Cell<bool>,
        // The playlist whose tracks are loading, to take the place of
        // those of the selected one once they're loaded
        pub pending_playlist: RefCell<Option<String>>,
        // Whether the sidebar was showing before the tracks of an album
        pub sidebar_was_visible: Cell<bool>,
        // Whether the sidebar was showing before the split view collapsed
        pub sidebar_visible_uncollapsed: Cell<bool>,

        pub queue_filtermodel: RefCell<Option<gio::ListModel>>,

        pub notify_playing_id: RefCell<Option<glib::SignalHandlerId>>,
        pub notify_position_id: RefCell<Option<glib::SignalHandlerId>>,
        pub notify_track_id: RefCell<Option<glib::SignalHandlerId>>,
        pub notify_cover_id: RefCell<Option<glib::SignalHandlerId>>,
        pub notify_ntracks_id: RefCell<Option<glib::SignalHandlerId>>,
        pub notify_current_id: RefCell<Option<glib::SignalHandlerId>>,
        pub notify_peaks_id: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "AmberolWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            PageSwitcher::static_type();
            Self::bind_template(klass);

            klass.install_action("win.play", None, move |win, _, _| {
                debug!("Window::win.play()");
                if let Some(p) = win.player() {
                    p.toggle_play();
                }
            });
            klass.install_action("win.seek-backwards", None, move |win, _, _| {
                debug!("Window::win.seek-backwards");
                if let Some(p) = win.player() {
                    p.seek_backwards();
                }
            });
            klass.install_action("win.seek-forward", None, move |win, _, _| {
                debug!("Window::win.seek-forward");
                if let Some(p) = win.player() {
                    p.seek_forward();
                }
            });
            klass.install_action("win.previous", None, move |win, _, _| {
                debug!("Window::win.previous()");
                if let Some(p) = win.player() {
                    p.skip_previous();
                }
            });
            klass.install_action("win.next", None, move |win, _, _| {
                debug!("Window::win.next()");
                if let Some(p) = win.player() {
                    p.skip_next();
                }
            });
            klass.install_action("queue.repeat-mode", None, move |win, _, _| {
                debug!("Window::queue.repeat()");
                if let Some(p) = win.player() {
                    p.toggle_repeat_mode();
                }
            });
            klass.install_action("win.copy", None, move |win, _, _| {
                debug!("Window::win.copy()");
                win.copy_track();
            });
            klass.install_action("win.track-info", None, move |win, _, _| {
                debug!("Window::win.track-info()");
                win.show_track_info();
            });
            klass.install_action_async("win.open-directory", None, async move |win, _, _| {
                debug!("Window::win.open-directory()");
                win.open_track_directory().await;
            });
            klass.install_action("queue.clear", None, move |win, _, _| {
                debug!("Window::queue.clear()");
                win.clear_queue();
            });
            klass.install_action(
                "win.select-playlist",
                Some(glib::VariantTy::STRING),
                move |win, _, param| {
                    debug!("Window::win.select-playlist()");
                    if let Some(name) = param.and_then(String::from_variant) {
                        win.set_playlists_visible(false);
                        win.select_playlist(&name);
                    }
                },
            );
            klass.install_action(
                "win.rename-playlist",
                Some(glib::VariantTy::STRING),
                move |win, _, param| {
                    debug!("Window::win.rename-playlist()");
                    if let Some(name) = param.and_then(String::from_variant) {
                        win.show_rename_playlist_dialog(&name);
                    }
                },
            );
            klass.install_action_async("win.import-playlist", None, async move |win, _, _| {
                debug!("Window::win.import-playlist()");
                win.import_playlist().await;
            });
            klass.install_action_async(
                "win.export-playlist",
                Some(glib::VariantTy::STRING),
                async move |win, _, param| {
                    debug!("Window::win.export-playlist()");
                    if let Some(name) = param.and_then(|p| p.get::<String>()) {
                        win.export_playlist(&name).await;
                    }
                },
            );
            klass.install_action(
                "win.delete-playlist",
                Some(glib::VariantTy::STRING),
                move |win, _, param| {
                    debug!("Window::win.delete-playlist()");
                    if let Some(name) = param.and_then(String::from_variant) {
                        win.show_delete_playlist_dialog(&name);
                    }
                },
            );
            klass.install_action("win.new-playlist", None, move |win, _, _| {
                debug!("Window::win.new-playlist()");
                win.show_new_playlist_dialog();
            });
            klass.install_action("queue.select-all", None, move |win, _, _| {
                debug!("Window::queue.select-all()");
                win.select_all();
            });
            klass.install_action("queue.remove-selected", None, move |win, _, _| {
                debug!("Window::queue.remove-selected()");
                win.remove_selected_tracks();
            });
            // Bindings instead of accelerators, which would take these keys
            // from the search entries: bindings only get the keys the focused
            // widget doesn't handle
            klass.add_binding_action(
                gdk::Key::Delete,
                gdk::ModifierType::empty(),
                "queue.remove-selected",
            );
            klass.add_binding_action(
                gdk::Key::KP_Delete,
                gdk::ModifierType::empty(),
                "queue.remove-selected",
            );

            // Esc leaves selection mode; when there is none, the key goes
            // on to do whatever it did before
            klass.add_binding(gdk::Key::Escape, gdk::ModifierType::empty(), |win| {
                if win.exit_selection_mode() {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });

            klass.install_property_action("queue.toggle", "playlist-visible");
            klass.install_property_action("queue.shuffle", "queue-shuffled");
            klass.install_property_action("queue.select", "queue-selection");
            klass.install_property_action("queue.search", "queue-search");
            klass.install_property_action("win.page", "page");
            klass.install_property_action("win.replaygain", "replaygain-mode");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }

        fn new() -> Self {
            Self {
                track_details: TemplateChild::default(),
                track_cover: TemplateChild::default(),
                split_view: TemplateChild::default(),
                toast_overlay: TemplateChild::default(),
                drag_overlay: TemplateChild::default(),
                playback_control: TemplateChild::default(),
                menu_button: TemplateChild::default(),
                waveform_view: TemplateChild::default(),
                elapsed_label: TemplateChild::default(),
                remaining_label: TemplateChild::default(),
                playlist_view: TemplateChild::default(),
                content_stack: TemplateChild::default(),
                library_view: TemplateChild::default(),
                playlists_page: TemplateChild::default(),
                sidebar_stack: TemplateChild::default(),
                album_tracks: TemplateChild::default(),
                sidebar_handle: TemplateChild::default(),
                queue_shuffled: Cell::new(false),
                playlist_visible: Cell::new(true),
                queue_selection: Cell::new(false),
                queue_anchor: glib::WeakRef::new(),
                queue_search: Cell::new(false),
                queue_filtermodel: RefCell::default(),
                replaygain_mode: Cell::new(ReplayGainMode::default()),
                sidebar_width: Cell::new(0),
                library_visible: Cell::new(false),
                playlists_visible: Cell::new(false),
                page: std::marker::PhantomData,
                player_available: Cell::new(false),
                starting: Cell::new(true),
                pending_playlist: RefCell::default(),
                hide_focus_after_transition: Cell::new(false),
                sidebar_was_visible: Cell::new(false),
                sidebar_visible_uncollapsed: Cell::new(true),
                provider: gtk::CssProvider::new(),
                settings: utils::settings_manager(),
                notify_playing_id: RefCell::new(None),
                notify_position_id: RefCell::new(None),
                notify_track_id: RefCell::new(None),
                notify_cover_id: RefCell::new(None),
                notify_ntracks_id: RefCell::new(None),
                notify_current_id: RefCell::new(None),
                notify_peaks_id: RefCell::new(None),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();

            if APPLICATION_ID.ends_with("Devel") {
                self.obj().add_css_class("devel");
            }

            // Only in selection mode
            self.obj()
                .action_set_enabled("queue.remove-selected", false);

            self.content_stack.connect_transition_running_notify(clone!(
                #[weak(rename_to = win)]
                self.obj(),
                move |stack| {
                    if !stack.is_transition_running()
                        && win.imp().hide_focus_after_transition.replace(false)
                    {
                        win.set_focus_visible(false);
                    }
                }
            ));

            // Each page has the main menu; F10 opens the one of the
            // visible page
            self.menu_button
                .set_menu_model(Some(&utils::primary_menu()));
            self.menu_button.set_primary(true);
        }
    }

    impl Window {
        fn set_playlist_visible(&self, visible: bool) {
            if visible != self.playlist_visible.replace(visible) {
                self.split_view.set_show_sidebar(visible);
                self.obj().notify_playlist_visible();
            }
        }

        fn set_queue_shuffled(&self, shuffled: bool) {
            let obj = self.obj();

            if shuffled != self.queue_shuffled.replace(shuffled) {
                if let Some(player) = obj.player() {
                    let queue = player.queue();
                    let state = player.state();
                    let reset_track = queue.is_first_track() && !state.playing();

                    queue.set_shuffled(shuffled);

                    if reset_track {
                        player.skip_to(0);
                    }
                }

                obj.notify_queue_shuffled();
            }
        }

        fn set_queue_selection(&self, selection: bool) {
            let obj = self.obj();

            if selection != self.queue_selection.replace(selection) {
                if !selection && let Some(player) = obj.player() {
                    let queue = player.queue();
                    queue.unselect_all_tracks();
                }
                if !selection {
                    self.queue_anchor.set(None);
                }

                self.playlist_view.queue_actionbar().set_revealed(selection);
                let queue_view = self.playlist_view.queue_view();
                if selection {
                    queue_view.add_css_class("selecting");
                } else {
                    queue_view.remove_css_class("selecting");
                }
                obj.action_set_enabled("queue.remove-selected", selection);

                obj.notify_queue_selection();
            }
        }

        fn set_queue_search(&self, search: bool) {
            let obj = self.obj();

            if search != self.queue_search.replace(search) {
                if search {
                    obj.set_playlist_visible(true);
                }
                self.playlist_view.set_search(search);
                obj.notify_queue_search();
            }
        }

        fn set_library_visible(&self, visible: bool) {
            let obj = self.obj();

            if !visible && obj.queue_is_empty() {
                // Keep the state of the action in sync with the grid, which
                // stays up
                obj.notify_library_visible();
                return;
            }
            if visible == self.library_visible.replace(visible) {
                return;
            }

            self.update_content_page();
            // Typing searches the albums right away
            if visible {
                self.library_view.focus_search();
            }
            obj.notify_library_visible();
        }

        fn set_playlists_visible(&self, visible: bool) {
            if visible == self.playlists_visible.replace(visible) {
                return;
            }

            let obj = self.obj();
            if visible {
                obj.update_playlists_page();
            }
            self.update_content_page();
            obj.notify_playlists_visible();
        }

        fn page(&self) -> String {
            if self.playlists_visible.get() {
                "playlists"
            } else if self.library_visible.get() {
                "library"
            } else {
                "player"
            }
            .to_string()
        }

        // The visibility of the pages changes before the stack does, so
        // that it goes straight to the page, rather than through the one
        // under the playlists
        fn set_page(&self, page: String) {
            let obj = self.obj();
            let old_page = self.page();
            let (library, playlists) = match page.as_str() {
                // Going to the albums from the playlists closes them
                "library" => (true, false),
                "playlists" => (self.library_visible.get(), true),
                // With nothing to play, there is no player to go to
                _ if obj.queue_is_empty() => {
                    (self.library_visible.get(), self.playlists_visible.get())
                }
                _ => (false, false),
            };
            let library_changed = self.library_visible.replace(library) != library;
            let playlists_changed = self.playlists_visible.replace(playlists) != playlists;

            if playlists && playlists_changed {
                obj.update_playlists_page();
            }
            self.update_content_page();
            // Typing searches the albums right away
            if self.page() == "library" && old_page != "library" {
                self.library_view.focus_search();
            }
            // The focus moves to the page, and the shortcuts would show it
            // there, like after any key; as after a click, it's shown again
            // when moving it with the keyboard. It can move again at the end
            // of the transition, when the previous page goes away
            if self.page() != old_page {
                obj.set_focus_visible(false);
                self.hide_focus_after_transition.set(true);
            }

            if library_changed {
                obj.notify_library_visible();
            }
            if playlists_changed {
                obj.notify_playlists_visible();
            }
            // A page that can't be shown, like the player with nothing to
            // play, leaves the buttons as they were
            obj.notify_page();
        }

        fn update_content_page(&self) {
            self.content_stack.set_visible_child_name(&self.page());
            self.obj().notify_page();
        }

        fn set_replaygain_mode(&self, replaygain: ReplayGainMode) {
            if replaygain != self.replaygain_mode.replace(replaygain) {
                let obj = self.obj();
                if let Some(p) = obj.player() {
                    p.set_replaygain(replaygain);
                }
                self.settings
                    .set_enum("replay-gain", replaygain.into())
                    .expect("Unable to store setting");

                obj.notify_replaygain_mode();
            }
        }
    }

    impl WidgetImpl for Window {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);

            // Keep the sidebar from eating into the space of the content
            // when the window gets narrower
            self.obj().update_sidebar_width();
        }
    }
    impl WindowImpl for Window {}
    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub fn new<P: IsA<gtk::Application>>(application: &P) -> Self {
        let win = glib::Object::builder::<Window>()
            .property("application", application)
            .build();

        win.setup_waveform();
        win.setup_actions();
        win.setup_playlist();
        win.setup_sidebar_handle();
        win.setup_drop_target();
        win.setup_select_all_shortcut();
        win.setup_library();
        win.setup_playlists();
        win.setup_provider();
        win.bind_state();
        win.bind_queue();
        win.connect_signals();
        win.restore_window_state();
        win.set_initial_state();
        win.set_default_widget(Some(&win.imp().playback_control.play_button()));

        win
    }

    fn player(&self) -> Option<Rc<AudioPlayer>> {
        if let Some(app) = self.application() {
            let player = app
                .downcast::<crate::application::Application>()
                .unwrap()
                .player();
            return Some(player);
        }

        None
    }

    fn setup_actions(&self) {
        self.add_action(&self.imp().settings.create_action("restore-playlist"));

        let enable_recoloring = self.imp().settings.boolean("enable-recoloring");
        self.add_action_entries([gio::ActionEntry::builder("enable-recoloring")
            .state(enable_recoloring.to_variant())
            .activate(|this: &Window, action, _| {
                let state = action.state().unwrap();
                let action_state: bool = state.get().unwrap();
                let enable_recoloring = !action_state;
                action.set_state(&enable_recoloring.to_variant());

                this.imp()
                    .settings
                    .set_boolean("enable-recoloring", enable_recoloring)
                    .expect("Unable to store setting");
            })
            .build()]);
    }

    fn restore_window_state(&self) {
        let settings = utils::settings_manager();
        let width = settings.int("window-width");
        let height = settings.int("window-height");
        self.set_default_size(width, height);
    }

    pub fn add_toast(&self, msg: String) {
        let toast = adw::Toast::new(&msg);
        self.imp().toast_overlay.add_toast(toast);
    }

    // The queue can change while the toast is up, so the track is
    // looked up when the button is clicked
    pub fn add_skip_to_toast(&self, msg: String, button: String, track: &Track) {
        let toast = adw::Toast::new(&msg);
        toast.set_button_label(Some(&button));
        toast.connect_button_clicked(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_| {
                if let Some(player) = win.player()
                    && let Some(pos) = player.queue().position(&track)
                {
                    player.skip_to(pos);
                    player.play();
                }
            }
        ));
        self.imp().toast_overlay.add_toast(toast);
    }
}
