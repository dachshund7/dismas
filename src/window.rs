// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod albums;
mod bindings;
mod loading;
mod now_playing;
mod playlist;
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
    playback_control::PlaybackControl,
    playlist_view::PlaylistView,
    queue_row::QueueRow,
    search::SearchFilter,
    track_cover::TrackCover,
    track_details::TrackDetails,
    track_info_dialog::TrackInfoDialog,
    utils,
    waveform_view::WaveformView,
    work_sections::{WorkSectionModel, work_header_factory},
};

// What happens once tracks are added to the playlist
#[derive(Clone, Copy, PartialEq)]
enum QueueMode {
    // Stay in the album grid, to keep browsing
    Add,
    // Show the player, e.g. after opening files
    Show,
    // Show the player, without announcing the tracks, for the playlist
    // of the last session
    Restore,
    // Show the player, and play the tracks from the given one
    Play(u32),
}

// Where the menu of a track is shown
#[derive(Clone, Copy)]
enum TrackMenu {
    Playlist,
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
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub playlist_view: TemplateChild<PlaylistView>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub library_view: TemplateChild<LibraryView>,
        #[template_child]
        pub sidebar_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub album_tracks: TemplateChild<AlbumTracks>,
        #[template_child]
        pub sidebar_handle: TemplateChild<gtk::Box>,

        pub provider: gtk::CssProvider,
        pub settings: gio::Settings,

        #[property(get, set = Self::set_playlist_shuffled, explicit_notify)]
        pub playlist_shuffled: Cell<bool>,
        #[property(get, set = Self::set_playlist_visible, explicit_notify)]
        pub playlist_visible: Cell<bool>,
        #[property(get, set = Self::set_playlist_selection, explicit_notify)]
        pub playlist_selection: Cell<bool>,
        #[property(get, set = Self::set_playlist_search, explicit_notify)]
        pub playlist_search: Cell<bool>,
        #[property(get, set = Self::set_replaygain_mode, explicit_notify, builder(ReplayGainMode::default()))]
        pub replaygain_mode: Cell<ReplayGainMode>,
        pub sidebar_width: Cell<i32>,
        #[property(get, set = Self::set_library_visible, explicit_notify)]
        pub library_visible: Cell<bool>,
        // Whether the playlist might still be restored or opened at startup
        pub starting: Cell<bool>,
        // Whether the sidebar was showing before the tracks of an album
        pub sidebar_was_visible: Cell<bool>,

        pub playlist_filtermodel: RefCell<Option<gio::ListModel>>,

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
                win.open_track_directory().await
            });
            klass.install_action("queue.clear", None, move |win, _, _| {
                debug!("Window::queue.clear()");
                win.clear_queue();
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
            klass.install_property_action("queue.shuffle", "playlist-shuffled");
            klass.install_property_action("queue.select", "playlist-selection");
            klass.install_property_action("queue.search", "playlist-search");
            klass.install_property_action("win.browse-albums", "library-visible");
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
                waveform_view: TemplateChild::default(),
                elapsed_label: TemplateChild::default(),
                remaining_label: TemplateChild::default(),
                playlist_view: TemplateChild::default(),
                content_stack: TemplateChild::default(),
                library_view: TemplateChild::default(),
                sidebar_stack: TemplateChild::default(),
                album_tracks: TemplateChild::default(),
                sidebar_handle: TemplateChild::default(),
                playlist_shuffled: Cell::new(false),
                playlist_visible: Cell::new(true),
                playlist_selection: Cell::new(false),
                playlist_search: Cell::new(false),
                playlist_filtermodel: RefCell::default(),
                replaygain_mode: Cell::new(ReplayGainMode::default()),
                sidebar_width: Cell::new(0),
                library_visible: Cell::new(false),
                starting: Cell::new(true),
                sidebar_was_visible: Cell::new(false),
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
        }
    }

    impl Window {
        fn set_playlist_visible(&self, visible: bool) {
            if visible != self.playlist_visible.replace(visible) {
                self.split_view.set_show_sidebar(visible);
                self.obj().notify_playlist_visible();
            }
        }

        fn set_playlist_shuffled(&self, shuffled: bool) {
            let obj = self.obj();

            if shuffled != self.playlist_shuffled.replace(shuffled) {
                if let Some(player) = obj.player() {
                    let queue = player.queue();
                    let state = player.state();
                    let reset_track = queue.is_first_track() && !state.playing();

                    queue.set_shuffled(shuffled);

                    if reset_track {
                        player.skip_to(0);
                    }
                }

                obj.notify_playlist_shuffled();
            }
        }

        fn set_playlist_selection(&self, selection: bool) {
            let obj = self.obj();

            if selection != self.playlist_selection.replace(selection) {
                if !selection && let Some(player) = obj.player() {
                    let queue = player.queue();
                    queue.unselect_all_tracks();
                }

                self.playlist_view.queue_actionbar().set_revealed(selection);
                obj.action_set_enabled("queue.remove-selected", selection);

                obj.notify_playlist_selection();
            }
        }

        fn set_playlist_search(&self, search: bool) {
            let obj = self.obj();

            if search != self.playlist_search.replace(search) {
                if search {
                    obj.set_playlist_visible(true);
                }
                self.playlist_view.set_search(search);
                obj.notify_playlist_search();
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

            let page = if visible { "library" } else { "player" };
            self.content_stack.set_visible_child_name(page);
            // Typing searches the albums right away
            if visible {
                self.library_view.focus_search();
            }
            obj.notify_library_visible();
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
            .build()])
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

    // The playlist can change while the toast is up, so the track is
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
