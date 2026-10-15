// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

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
    audio::{AudioPlayer, CoverCache, RepeatMode, ReplayGainMode, Track},
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

// The width of the sidebar when the user resizes it
const MIN_SIDEBAR_WIDTH: i32 = 240;

// The width of the sidebar when it's sized automatically
const AUTO_SIDEBAR_WIDTH: f64 = 330.0;
const AUTO_SIDEBAR_FRACTION: f64 = 0.4;

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

const ATTRIBUTE_HOST_PATH: &str = "xattr::document-portal.host-path";

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

            klass.install_action(
                "win.skip-to",
                Some(glib::VariantTy::UINT32),
                move |win, _, param| {
                    if let Some(pos) = param.and_then(u32::from_variant)
                        && let Some(player) = win.player()
                    {
                        player.skip_to(pos);
                        player.play();
                    }
                },
            );
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

    fn setup_waveform(&self) {
        if let Some(player) = self.player() {
            let peaks = player.waveform_generator().peaks();
            self.imp().waveform_view.set_peaks(peaks);

            let notify_peaks_id = player.waveform_generator().connect_notify_local(
                Some("has-peaks"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |generator, _| {
                        let peaks = generator.peaks();
                        win.imp().waveform_view.set_peaks(peaks);
                    }
                ),
            );
            self.imp().notify_peaks_id.replace(Some(notify_peaks_id));
        }
    }

    fn unbind_waveform(&self) {
        if let Some(player) = self.player()
            && let Some(id) = self.imp().notify_peaks_id.take()
        {
            player.waveform_generator().disconnect(id);
        }
    }

    fn restore_window_state(&self) {
        let settings = utils::settings_manager();
        let width = settings.int("window-width");
        let height = settings.int("window-height");
        self.set_default_size(width, height);
    }

    fn setup_sidebar_handle(&self) {
        let imp = self.imp();

        imp.sidebar_width.set(imp.settings.int("sidebar-width"));
        imp.sidebar_handle.set_cursor_from_name(Some("col-resize"));

        // The whole window is a GtkWindowHandle, so the gestures on the
        // handle need to claim their events, or the window would be moved
        // instead of the sidebar being resized
        let drag = gtk::GestureDrag::new();
        let start_width = Rc::new(Cell::new(0));
        let start_x = Rc::new(Cell::new(0.0));
        let last_press = Rc::new(Cell::new(0u32));
        // Whether the current press reset the width, instead of starting
        // a drag
        let was_reset = Rc::new(Cell::new(false));
        let was_dragged = Rc::new(Cell::new(false));
        drag.connect_drag_begin(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            start_width,
            #[strong]
            start_x,
            #[strong]
            last_press,
            #[strong]
            was_reset,
            move |gesture, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);

                // A double click resets the width; we cannot use a separate
                // GtkGestureClick for it, as the drag gesture would reset its
                // count of presses
                let time = gesture.current_event_time();
                let double_click_time = win.settings().gtk_double_click_time() as u32;
                let previous = last_press.replace(time);
                let is_double_click =
                    previous != 0 && time.wrapping_sub(previous) <= double_click_time;
                was_reset.set(is_double_click);
                if is_double_click {
                    last_press.set(0);
                    win.imp().sidebar_width.set(0);
                    win.update_sidebar_width();
                    win.store_sidebar_width();
                    return;
                }

                let imp = win.imp();
                if let Some(sidebar) = imp.split_view.sidebar() {
                    start_width.set(sidebar.width());
                }
                start_x.set(win.handle_x_in_window(x, y));
                imp.sidebar_handle.add_css_class("dragging");
            }
        ));
        drag.connect_drag_update(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            start_width,
            #[strong]
            start_x,
            #[strong]
            was_reset,
            #[strong]
            was_dragged,
            move |gesture, _, _| {
                if was_reset.get() {
                    return;
                }

                // The handle moves while the sidebar is resized, so we
                // track the pointer in window coordinates instead of
                // using the offset relative to the handle; pointer events
                // have no sequence, so we cannot bail out if there isn't one
                let sequence = gesture.current_sequence();
                let Some((x, y)) = gesture.point(sequence.as_ref()) else {
                    return;
                };
                let offset_x = win.handle_x_in_window(x, y) - start_x.get();

                // The sidebar is at the start, so in RTL locales the
                // handle is on its left edge
                let offset_x = if win.direction() == gtk::TextDirection::Rtl {
                    -offset_x
                } else {
                    offset_x
                };

                if offset_x != 0.0 {
                    was_dragged.set(true);
                }

                let width = start_width.get() + offset_x.round() as i32;
                win.imp().sidebar_width.set(width.max(MIN_SIDEBAR_WIDTH));
                win.update_sidebar_width();
            }
        ));
        drag.connect_drag_end(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            was_dragged,
            move |_, _, _| {
                let imp = win.imp();
                imp.sidebar_handle.remove_css_class("dragging");

                // Nothing to store for a click without a drag, or for the
                // double click that reset the width
                if !was_dragged.replace(false) {
                    return;
                }

                // Store the width the sidebar actually has, in case it
                // was clamped to leave room for the content
                if let Some(sidebar) = imp.split_view.sidebar() {
                    imp.sidebar_width.set(sidebar.width());
                }
                win.store_sidebar_width();
            }
        ));
        imp.sidebar_handle.add_controller(drag);

        self.update_sidebar_width();
    }

    // Translates a point in the coordinates of the sidebar handle to the
    // horizontal coordinate in the window
    fn handle_x_in_window(&self, x: f64, y: f64) -> f64 {
        let handle = &self.imp().sidebar_handle;
        handle
            .compute_point(self, &graphene::Point::new(x as f32, y as f32))
            .map(|p| p.x() as f64)
            .unwrap_or(x)
    }

    fn update_sidebar_width(&self) {
        let split_view = &self.imp().split_view;
        let wanted = self.imp().sidebar_width.get();

        // The automatic width scales with the text, like the rest of the
        // split view; the width set by the user is in pixels, as it's
        // measured from the pointer position
        let unit = if wanted <= 0 {
            adw::LengthUnit::Sp
        } else {
            adw::LengthUnit::Px
        };
        if split_view.sidebar_width_unit() != unit {
            split_view.set_sidebar_width_unit(unit);
        }

        let (max_width, fraction) = if wanted <= 0 {
            (AUTO_SIDEBAR_WIDTH, AUTO_SIDEBAR_FRACTION)
        } else {
            let total = split_view.width();
            let available = if split_view.is_collapsed() {
                total
            } else {
                let content_min = split_view
                    .content()
                    .map(|c| c.measure(gtk::Orientation::Horizontal, -1).0)
                    .unwrap_or(0);
                total - content_min
            };

            // The window has not been allocated yet
            let width = if total > 0 {
                wanted.min(available).max(MIN_SIDEBAR_WIDTH)
            } else {
                wanted
            };

            (width as f64, 1.0)
        };

        // Only update the split view if something changed, as each change
        // queues a new resize
        if split_view.max_sidebar_width() != max_width {
            split_view.set_max_sidebar_width(max_width);
        }
        if split_view.sidebar_width_fraction() != fraction {
            split_view.set_sidebar_width_fraction(fraction);
        }
    }

    fn store_sidebar_width(&self) {
        let width = self.imp().sidebar_width.get();
        if let Err(e) = self.imp().settings.set_int("sidebar-width", width) {
            debug!("Unable to store sidebar-width: {}", e);
        }
    }

    fn reset_queue(&self) {
        self.set_playlist_visible(false);
        self.set_playlist_shuffled(false);
        self.set_playlist_selection(false);
        self.update_style(None);
    }

    fn clear_queue(&self) {
        if let Some(p) = self.player() {
            p.clear_queue();

            // So that the playlist isn't restored at the next startup
            utils::store_playlist(p.queue());
        }
    }

    // Shows the tracks of an album in the sidebar, in place of the
    // playlist, so that it can be played or added to the playlist
    fn show_album_tracks(&self, album: &Album, search_words: &[String]) {
        let imp = self.imp();
        if imp.sidebar_stack.visible_child_name().as_deref() != Some("album") {
            imp.sidebar_was_visible.set(self.playlist_visible());
        }
        imp.album_tracks.show_album(album, search_words);
        imp.sidebar_stack.set_visible_child_name("album");
        self.set_playlist_visible(true);
    }

    // Shows an album in the grid, with all its tracks in the sidebar
    fn go_to_album(&self, album_id: &str) {
        // Showing the grid focuses the search, so it goes first, for the
        // album to take the focus
        self.set_library_visible(true);
        let Some(album) = self.imp().library_view.reveal_album(album_id) else {
            return;
        };
        self.show_album_tracks(&album, &[]);
    }

    // The menu of a track, at the given point of its row
    fn show_track_menu(&self, row: &QueueRow, x: f64, y: f64, kind: TrackMenu) {
        let Some(track) = row.track() else {
            return;
        };
        let album_id = track.file().path().and_then(|p| library::track_album(&p));

        let actions = gio::SimpleActionGroup::new();
        let go_to_album = gio::SimpleAction::new("go-to-album", None);
        go_to_album.set_enabled(album_id.is_some());
        go_to_album.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(album_id) = &album_id {
                    win.go_to_album(album_id);
                }
            }
        ));
        actions.add_action(&go_to_album);

        let info = gio::SimpleAction::new("info", None);
        info.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.show_track_info_for(&track)
        ));
        actions.add_action(&info);

        let remove = gio::SimpleAction::new("remove", None);
        remove.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| {
                win.remove_track(&track);
                if let Some(player) = win.player() {
                    utils::store_playlist(player.queue());
                }
            }
        ));
        actions.add_action(&remove);

        let add = gio::SimpleAction::new("add", None);
        // Like adding the whole album, not while tracks are being added
        add.set_enabled(self.imp().album_tracks.adding_enabled());
        add.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.queue_tracks(vec![track.file()], QueueMode::Add)
        ));
        actions.add_action(&add);

        let select = gio::SimpleAction::new("select", None);
        select.set_enabled(match kind {
            TrackMenu::Playlist => !self.playlist_selection(),
            TrackMenu::Album => !self.imp().album_tracks.selection_mode(),
        });
        select.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| match kind {
                TrackMenu::Playlist => win.toggle_playlist_track(&track),
                TrackMenu::Album => win.imp().album_tracks.toggle_track(&track),
            }
        ));
        actions.add_action(&select);

        let menu = gio::Menu::new();
        menu.append(Some(&i18n("Go to Album")), Some("track.go-to-album"));
        menu.append(Some(&i18n("Track Information")), Some("track.info"));
        menu.append(Some(&i18n("Select")), Some("track.select"));
        let section = gio::Menu::new();
        match kind {
            TrackMenu::Playlist => {
                section.append(Some(&i18n("Remove from Playlist")), Some("track.remove"))
            }
            TrackMenu::Album => section.append(Some(&i18n("Add to Playlist")), Some("track.add")),
        }
        menu.append_section(None, &section);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.insert_action_group("track", Some(&actions));
        popover.set_parent(row);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        // The menu item is activated after the popover closes, so it can
        // only go once the action ran
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    // Goes back from the tracks of an album to the playlist, closing the
    // sidebar if it was opened for the album
    fn show_playlist(&self) {
        self.show_playlist_pane();
        if !self.imp().sidebar_was_visible.get() {
            self.set_playlist_visible(false);
        }
    }

    fn show_playlist_pane(&self) {
        self.imp().sidebar_stack.set_visible_child_name("playlist");
    }

    fn queue_is_empty(&self) -> bool {
        self.player().is_none_or(|p| p.queue().is_empty())
    }

    // With an empty playlist, there is nothing to play, so the album grid
    // takes the place of the player, and there's no going back to it
    fn update_library_for_queue(&self) {
        let queue_empty = self.queue_is_empty();
        self.imp().library_view.set_can_go_back(!queue_empty);

        // At startup, the playlist is empty until it's restored, or until
        // the files to open are added, and showing the grid in the meantime
        // would only make it flash
        if queue_empty && !self.imp().starting.get() {
            self.set_library_visible(true);
            self.set_playlist_visible(false);
        }
    }

    // Adds the tracks of the albums selected in the grid to the playlist,
    // or plays them in its place; for the albums selected in a search, only
    // the tracks that match it
    fn queue_selected_albums(&self, play: bool) {
        let library_view = self.imp().library_view.get();
        let albums = library_view.selected_albums();
        library_view.set_selection_mode(false);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Ok(paths) = gio::spawn_blocking(move || {
                    albums
                        .iter()
                        .flat_map(|(id, words)| library::album_tracks(id, words))
                        .collect::<Vec<_>>()
                })
                .await
                else {
                    return;
                };
                let files = paths.into_iter().map(gio::File::for_path).collect();

                if play {
                    win.clear_queue();
                    win.show_playlist_pane();
                    win.queue_tracks(files, QueueMode::Play(0));
                } else {
                    win.queue_tracks(files, QueueMode::Add);
                }
            }
        ));
    }

    // Called once the playlist of the last session, or the files the
    // application was opened with, have been added, or there are none
    fn end_startup(&self) {
        let imp = self.imp();
        if !imp.starting.replace(false) {
            return;
        }

        // The window is already up, but nothing was shown in the meantime,
        // so there is nothing to fade from
        let transition = imp.content_stack.transition_type();
        imp.content_stack
            .set_transition_type(gtk::StackTransitionType::None);
        self.update_library_for_queue();
        imp.content_stack.set_transition_type(transition);
    }

    fn setup_library(&self) {
        let library_view = self.imp().library_view.get();

        library_view.connect_album_activated(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, album| {
                win.show_album_tracks(album, &win.imp().library_view.search_words());
            }
        ));

        let album_tracks = self.imp().album_tracks.get();
        album_tracks.connect_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, row, x, y| {
                win.show_track_menu(row, x, y, TrackMenu::Album);
            }
        ));
        album_tracks.connect_back(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.show_playlist()
        ));
        album_tracks.connect_add(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks| {
                win.show_playlist_pane();
                win.queue_tracks(album_tracks.files(), QueueMode::Add);
            }
        ));
        album_tracks.connect_play(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks, start| {
                win.clear_queue();
                win.show_playlist_pane();
                win.queue_tracks(album_tracks.files(), QueueMode::Play(start));
            }
        ));

        library_view.connect_add_albums(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.queue_selected_albums(false)
        ));
        library_view.connect_play_albums(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.queue_selected_albums(true)
        ));

        // Reading the albums can take a while, so we do it in the
        // background as soon as the window is up
        library_view.start_scan();
    }

    // Restores the playlist of the last session, if the user wants it
    pub fn restore_playlist_on_startup(&self) {
        if self.imp().settings.boolean("restore-playlist")
            && utils::has_cached_playlist()
            && self.queue_is_empty()
        {
            self.restore_playlist();
        } else {
            self.end_startup();
        }
    }

    fn restore_playlist(&self) {
        // The saved playlist is empty after clearing the playlist
        match utils::load_cached_tracks() {
            Some(tracks) if !tracks.is_empty() => self.queue_tracks(tracks, QueueMode::Restore),
            _ => self.end_startup(),
        }
    }

    // Adds the files to the end of the playlist
    fn queue_tracks(&self, queue: Vec<gio::File>, mode: QueueMode) {
        let play = matches!(mode, QueueMode::Play(_));
        if queue.is_empty() {
            self.add_toast(i18n("No available track found"));
            self.end_startup();
            return;
        }

        // Disable actions on the queue; loading is "atomic"
        self.action_set_enabled("queue.clear", false);
        self.imp().album_tracks.set_adding_enabled(false);
        self.imp().library_view.set_adding_enabled(false);

        self.set_playlist_visible(true);
        self.imp().playlist_view.begin_loading();

        // Begin the trace
        let now = Instant::now();

        // Turn the list of files into tracks one at a time into the main loop
        let n_files = queue.len() as u32;

        let mut files = queue.into_iter();
        let mut tracks = Vec::new();
        let mut cur_file: u32 = 0;
        let mut duplicates: u32 = 0;

        glib::idle_add_local(clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                files
                    .next()
                    .map(|f| {
                        win.imp().playlist_view.update_loading(cur_file, n_files);
                        if let Ok(s) = Track::from_uri(f.uri().as_str())
                            && let Some(player) = win.player()
                        {
                            let queue = player.queue();
                            if queue.contains(&s) {
                                duplicates += 1;
                            } else {
                                tracks.push(s);
                                cur_file += 1;
                            }
                        }
                    })
                    .map(|_| glib::ControlFlow::Continue)
                    .unwrap_or_else(|| {
                        debug!(
                            "Total loading time for {} files: {} ms",
                            n_files,
                            now.elapsed().as_millis()
                        );

                        // Re-enable the actions
                        win.action_set_enabled("queue.clear", true);
                        win.imp().album_tracks.set_adding_enabled(true);
                        win.imp().library_view.set_adding_enabled(true);

                        if tracks.is_empty() {
                            if duplicates == 0 {
                                win.add_toast(i18n("No tracks found"));
                            }
                        } else if let Some(player) = win.player() {
                            let queue = player.queue();
                            let was_empty = queue.is_empty();
                            let first_added = queue.n_tracks();

                            win.imp().playlist_view.end_loading();

                            // Bulk add to avoid hammering the UI with list model updates
                            queue.add_tracks(&tracks);

                            // Store the current state of the playlist
                            utils::store_playlist(queue);

                            debug!(
                                "Queue was empty: {}, new size: {}",
                                was_empty,
                                queue.n_tracks()
                            );
                            if let QueueMode::Play(start) = mode {
                                player.skip_to(first_added + start);
                                player.play();
                            } else if was_empty {
                                player.skip_to(0);
                            }
                            if mode == QueueMode::Add {
                                // Show where the album went, even if the
                                // playlist was closed while adding it
                                win.set_playlist_visible(true);
                            } else {
                                win.set_library_visible(false);
                            }

                            // Restoring the playlist at startup is quiet, and
                            // doesn't start playing
                            if mode == QueueMode::Restore {
                                // Nothing to announce
                            } else if tracks.len() == 1 {
                                // Allow jumping to the track we just added
                                // If we added a single track, and the queue was empty, we
                                // dispense with the pleasantries and we start playing
                                // immediately; otherwise, we let the user choose whether
                                // to jump to the newly added track
                                if was_empty || play {
                                    player.play();
                                } else {
                                    win.add_skip_to_toast(
                                        i18n("Added a new track"),
                                        i18n("Play"),
                                        queue.n_tracks() - 1,
                                    );
                                }
                            } else {
                                let msg = ni18n_f(
                                    // Translators: the `{}` must be left unmodified;
                                    // it will be expanded to the number of tracks added
                                    // to the playlist
                                    "Added one track",
                                    "Added {} tracks",
                                    tracks.len() as u32,
                                    &[&tracks.len().to_string()],
                                );

                                win.add_toast(msg);
                            }
                        }

                        win.end_startup();
                        glib::ControlFlow::Break
                    })
            }
        ));
    }

    fn add_files_to_queue(&self, model: &gio::ListModel) {
        let mut queue: Vec<gio::File> = vec![];

        for pos in 0..model.n_items() {
            let file = model.item(pos).unwrap().downcast::<gio::File>().unwrap();

            if let Ok(info) = file.query_info(
                "standard::name,standard::display-name,standard::type,standard::content-type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                gio::Cancellable::NONE,
            ) {
                match info.file_type() {
                    gio::FileType::Regular => {
                        if let Some(content_type) = info.content_type()
                            && gio::content_type_is_mime_type(&content_type, "audio/*")
                        {
                            debug!("Adding file '{}' to the queue", file.uri());
                            queue.push(file);
                        }
                    }
                    gio::FileType::Directory => {
                        debug!("Adding folder '{}' to the queue", file.uri());
                        let files = utils::load_files_from_folder(&file, true);
                        queue.extend(files);
                    }
                    _ => (),
                }
            }
        }

        self.queue_tracks(queue, QueueMode::Show);
    }

    // Bind the PlayerState to the UI
    fn bind_state(&self) {
        let imp = self.imp();
        if let Some(player) = self.player() {
            let state = player.state();

            // Use the PlayerState:playing property to control the play/pause button
            self.update_play_button();
            let notify_playing_id = state.connect_notify_local(
                Some("playing"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.set_playlist_selection(false);
                        win.update_play_button();
                    }
                ),
            );
            imp.notify_playing_id.replace(Some(notify_playing_id));

            // Update the position labels
            self.update_position_labels();
            let notify_position_id = state.connect_notify_local(
                Some("position"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_position_labels();
                    }
                ),
            );
            imp.notify_position_id.replace(Some(notify_position_id));

            // Update the UI
            self.update_track();
            let notify_track_id = state.connect_notify_local(
                Some("track"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_track();
                    }
                ),
            );
            imp.notify_track_id.replace(Some(notify_track_id));

            // Update the cover, if any is available
            self.update_cover();
            let notify_cover_id = state.connect_notify_local(
                Some("cover"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_cover();
                    }
                ),
            );
            imp.notify_cover_id.replace(Some(notify_cover_id));

            // Bind the track properties to the UI
            state
                .bind_property("heading", &imp.track_details.get().title_label(), "label")
                .sync_create()
                .build();
            state
                .bind_property("artist", &imp.track_details.get().artist_label(), "label")
                .sync_create()
                .build();
            state
                .bind_property("album", &imp.track_details.get().album_label(), "label")
                .sync_create()
                .build();
            state
                .bind_property(
                    "movement",
                    &imp.track_details.get().movement_label(),
                    "label",
                )
                .sync_create()
                .build();
            state
                .bind_property(
                    "movement",
                    &imp.track_details.get().movement_label(),
                    "visible",
                )
                .transform_to(|_, movement: Option<String>| Some(movement.is_some()))
                .sync_create()
                .build();
            state
                .bind_property("credits", &imp.track_details.get().credits_label(), "label")
                .sync_create()
                .build();
            // Most tracks have no credits, so we hide the line instead of
            // leaving an empty space
            state
                .bind_property(
                    "credits",
                    &imp.track_details.get().credits_label(),
                    "visible",
                )
                .transform_to(|_, credits: Option<String>| {
                    Some(credits.is_some_and(|c| !c.is_empty()))
                })
                .sync_create()
                .build();
            state
                .bind_property(
                    "volume",
                    &imp.playback_control.get().volume_control(),
                    "volume",
                )
                .sync_create()
                .build();
        }
    }

    fn unbind_state(&self) {
        if let Some(player) = self.player() {
            let state = player.state();

            if let Some(id) = self.imp().notify_playing_id.take() {
                state.disconnect(id);
            }
            if let Some(id) = self.imp().notify_position_id.take() {
                state.disconnect(id);
            }
            if let Some(id) = self.imp().notify_track_id.take() {
                state.disconnect(id);
            }
            if let Some(id) = self.imp().notify_cover_id.take() {
                state.disconnect(id);
            }
        }
    }

    // Bind the Queue to the UI
    fn bind_queue(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();

            let notify_ntracks_id = queue.connect_notify_local(
                Some("n-tracks"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |queue, _| {
                        debug!("queue.n_tracks() = {}", queue.n_tracks());
                        if queue.is_empty() {
                            win.set_initial_state();
                            win.reset_queue();
                        } else {
                            win.update_library_for_queue();
                            win.action_set_enabled("queue.shuffle", queue.n_tracks() > 1);

                            win.action_set_enabled("win.play", true);
                            win.action_set_enabled("win.previous", true);
                            win.action_set_enabled("win.next", queue.n_tracks() > 1);
                        }

                        if queue.n_tracks() == 1
                            && let Some(p) = win.player()
                        {
                            p.skip_next();
                        }

                        win.update_playlist_time();
                    }
                ),
            );
            self.imp()
                .notify_ntracks_id
                .replace(Some(notify_ntracks_id));

            queue.connect_notify_local(
                Some("repeat-mode"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |queue, _| {
                        win.imp()
                            .playback_control
                            .set_repeat_mode(queue.repeat_mode());
                    }
                ),
            );

            let notify_current_id = queue.connect_notify_local(
                Some("current"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |queue, _| {
                        if queue.is_last_track() {
                            match queue.repeat_mode() {
                                RepeatMode::Consecutive => {
                                    win.action_set_enabled("win.next", false);
                                }
                                _ => {
                                    win.action_set_enabled("win.next", true);
                                }
                            }
                        } else {
                            win.action_set_enabled("win.next", true);
                        }
                    }
                ),
            );
            self.imp()
                .notify_current_id
                .replace(Some(notify_current_id));
        }
    }

    fn unbind_queue(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            if let Some(id) = self.imp().notify_ntracks_id.take() {
                queue.disconnect(id);
            }
            if let Some(id) = self.imp().notify_current_id.take() {
                queue.disconnect(id);
            }
        }
    }

    fn connect_signals(&self) {
        self.imp().split_view.connect_notify_local(
            Some("collapsed"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |split_view, _| {
                    win.set_playlist_visible(split_view.shows_sidebar());
                    win.imp()
                        .playlist_view
                        .back_button()
                        .set_visible(split_view.is_collapsed());
                }
            ),
        );

        self.imp().split_view.connect_notify_local(
            Some("show-sidebar"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |split_view, _| {
                    win.set_playlist_visible(split_view.shows_sidebar());
                }
            ),
        );

        self.imp().waveform_view.connect_position_changed(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, position| {
                debug!("New position: {}", position);
                if let Some(player) = win.player() {
                    let state = player.state();
                    if state.track().is_some() {
                        player.seek_position_rel(position);
                        player.play();
                    }
                }
            }
        ));

        self.imp()
            .playback_control
            .volume_control()
            .connect_volume_changed(clone!(
                #[weak(rename_to = win)]
                self,
                move |_, volume| {
                    debug!("Volume changed: {}", volume);
                    if let Some(p) = win.player() {
                        p.set_volume(volume);
                    }
                }
            ));

        self.imp()
            .playlist_view
            .queue_select_all_button()
            .connect_clicked(clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.select_all_playlist_tracks()
            ));

        self.imp()
            .playlist_view
            .playlist_searchbar()
            .connect_notify_local(
                Some("search-mode-enabled"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |searchbar, _| {
                        win.set_playlist_search(searchbar.is_search_mode());
                    }
                ),
            );

        self.imp().settings.connect_changed(
            Some("enable-recoloring"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |settings, _| {
                    debug!(
                        "GSettings:enable-recoloring: {}",
                        settings.boolean("enable-recoloring")
                    );
                    if let Some(player) = this.player() {
                        let state = player.state();
                        this.update_style(state.track().as_ref());
                    }
                }
            ),
        );
        let _dummy = self.imp().settings.boolean("enable-recoloring");

        self.connect_close_request(move |window| {
            debug!("Saving window state");
            let width = window.default_size().0;
            let height = window.default_size().1;

            let settings = utils::settings_manager();
            settings
                .set_int("window-width", width)
                .expect("Unable to store window-width");
            settings
                .set_int("window-height", height)
                .expect("Unable to stop window-height");

            window.unbind_queue();
            window.unbind_state();
            window.unbind_waveform();

            glib::Propagation::Proceed
        });

        self.imp()
            .playlist_view
            .playlist_searchbar()
            .set_key_capture_widget(Some(self.upcast_ref::<gtk::Widget>()));
    }

    // The initial state of the playback actions
    fn set_initial_state(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            self.action_set_enabled("win.play", !queue.is_empty());
            self.action_set_enabled("win.previous", !queue.is_empty());
            self.action_set_enabled("win.next", !queue.is_last_track());

            self.action_set_enabled("queue.shuffle", queue.n_tracks() > 1);
            self.action_set_enabled("win.replaygain", player.replaygain_available());

            let replaygain = self.imp().settings.enum_("replay-gain").into();
            self.set_replaygain_mode(replaygain);

            // Manually set player state, because set_replaygain
            // only updates player state when the value changes.
            player.set_replaygain(replaygain);

            self.imp()
                .playback_control
                .set_repeat_mode(queue.repeat_mode());
            self.set_playlist_shuffled(queue.is_shuffled());

            self.update_library_for_queue();
        }
    }

    fn setup_playlist(&self) {
        let imp = self.imp();

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, item| {
                let row = QueueRow::default();
                let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();
                list_item.set_child(Some(&row));

                row.connect_notify_local(
                    Some("selected"),
                    clone!(
                        #[weak]
                        win,
                        move |_, _| {
                            win.update_selected_count();
                        }
                    ),
                );

                row.connect_menu_requested(clone!(
                    #[weak]
                    win,
                    move |row, x, y| win.show_track_menu(row, x, y, TrackMenu::Playlist)
                ));
                row.connect_select_requested(clone!(
                    #[weak]
                    win,
                    move |row| {
                        if let Some(track) = row.track() {
                            win.toggle_playlist_track(&track);
                        }
                    }
                ));

                win.bind_property("playlist-selection", &row, "selection-mode")
                    .sync_create()
                    .build();

                row.bind_list_item(list_item);
            }
        ));
        imp.playlist_view
            .queue_view()
            .set_factory(Some(&factory.upcast::<gtk::ListItemFactory>()));
        imp.playlist_view
            .queue_view()
            .set_header_factory(Some(&work_header_factory()));

        if let Some(player) = self.player() {
            let queue = player.queue();

            let filter = SearchFilter::new();
            let filter_model =
                gtk::FilterListModel::new(Some(queue.model().clone()), Some(filter.clone()));
            // Search results keep the order of the playlist
            let sections = WorkSectionModel::new(&filter_model);
            let selection = gtk::NoSelection::new(Some(sections));
            imp.playlist_view
                .queue_view()
                .set_model(Some(selection.upcast_ref::<gtk::SelectionModel>()));
            imp.playlist_view.queue_view().connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                selection,
                move |_, pos| {
                    let track = selection
                        .upcast::<gio::ListModel>()
                        .item(pos)
                        .unwrap()
                        .downcast::<Track>()
                        .unwrap();

                    if let Some(player) = win.player() {
                        let queue = player.queue();

                        let mut real_pos = None;
                        for i in 0..queue.model().n_items() {
                            if let Some(item) = queue.model().item(i) {
                                let s = item.downcast::<Track>().unwrap();
                                if s.equals(&track) {
                                    real_pos = Some(i);
                                    break;
                                }
                            }
                        }

                        if let Some(real_pos) = real_pos {
                            if win.playlist_selection() {
                                queue.select_track_at(real_pos);
                            } else if queue.current_track_index() != Some(real_pos) {
                                player.skip_to(real_pos);
                                player.play();
                            } else if !player.state().playing() {
                                player.play();
                            }
                        }
                    }
                }
            ));

            imp.playlist_filtermodel
                .replace(Some(filter_model.upcast::<gio::ListModel>()));

            imp.playlist_view
                .playlist_searchentry()
                .bind_property("text", &filter, "search")
                .sync_create()
                .build();
            imp.playlist_view
                .playlist_searchentry()
                .connect_search_changed(clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_| {
                        if let Some(adjustment) = win.imp().playlist_view.queue_view().vadjustment()
                        {
                            adjustment.set_value(0.0);
                        }
                    }
                ));
        }
    }

    // Ctrl+A selects the items of the pane with the focus, even from its
    // search entry, so it's handled before the focused widget gets it; the
    // grid and the lists would otherwise take it for their own selection,
    // which does nothing, and the search entries for selecting their text
    fn setup_select_all_shortcut(&self) {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        controller.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>a"),
            Some(gtk::NamedAction::new("queue.select-all")),
        ));
        self.add_controller(controller);
    }

    fn setup_drop_target(&self) {
        let drop_target = gtk::DropTarget::builder()
            .name("file-drop-target")
            .actions(gdk::DragAction::COPY)
            .formats(&gdk::ContentFormats::for_type(gdk::FileList::static_type()))
            .build();

        drop_target.connect_drop(clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                if let Ok(file_list) = value.get::<gdk::FileList>() {
                    if file_list.files().is_empty() {
                        win.add_toast(i18n("Unable to access dropped files"));
                        return false;
                    }

                    let model = gio::ListStore::new::<gio::File>();
                    for f in file_list.files() {
                        model.append(&f);
                    }
                    win.add_files_to_queue(model.upcast_ref::<gio::ListModel>());
                    return true;
                }

                false
            }
        ));

        self.imp().drag_overlay.set_drop_target(Some(&drop_target));
    }

    fn update_play_button(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            let play_button = self.imp().playback_control.play_button();
            if state.playing() {
                play_button.set_icon_name("media-playback-pause-symbolic");
            } else {
                play_button.set_icon_name("media-playback-start-symbolic");
            }
        }
    }

    fn update_position_labels(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            if state.track().is_some() {
                let elapsed = state.position();
                let duration = state.duration();
                let remaining = duration.saturating_sub(elapsed);
                self.set_track_time(Some(elapsed), Some(remaining));

                let position = state.position() as f64 / state.duration() as f64;
                self.set_track_position(position);
            } else {
                self.set_track_time(None, None);
                self.set_track_position(0.0);
            }
        }
    }

    fn update_track(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            self.scroll_playlist_to_track();
            self.update_playlist_time();
            self.update_title(state.track().as_ref());
            self.update_style(state.track().as_ref());
        }
    }

    fn update_cover(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            let track_cover = self.imp().track_cover.get();
            if let Some(cover) = state.cover() {
                track_cover.album_image().set_cover(Some(&cover));
                track_cover.show_cover_image(true);

                if let Some(track) = state.track() {
                    self.load_full_cover(&track);
                }
            } else {
                track_cover.album_image().set_cover(None::<&gdk::Texture>);
                track_cover.show_cover_image(false);
            }
        }
    }

    // The cover shared by all the tracks of an album is too small for the
    // main view, so we replace it with the full resolution cover of the
    // current track, once it has been loaded in a separate thread
    fn load_full_cover(&self, track: &Track) {
        let Some(path) = track.file().path() else {
            return;
        };
        let uri = track.uri();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let cover = gio::spawn_blocking(move || CoverCache::load_full_cover(&path))
                    .await
                    .ok()
                    .flatten();
                let Some(cover) = cover else {
                    return;
                };

                // The track might have changed while we were loading
                let Some(player) = win.player() else {
                    return;
                };
                if player.state().track().map(|t| t.uri()) != Some(uri) {
                    return;
                }

                win.imp()
                    .track_cover
                    .get()
                    .album_image()
                    .set_cover(Some(&cover.texture()));
            }
        ));
    }

    fn update_playlist_time(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            let n_tracks = queue.n_tracks();
            if let Some(current) = queue.current_track_index() {
                let mut remaining_time = 0;
                for pos in 0..n_tracks {
                    let track = queue.track_at(pos).unwrap();
                    if pos >= current {
                        remaining_time += track.duration();
                    }
                }

                let remaining_min = ((remaining_time - (remaining_time % 60)) / 60) as u32;
                let remaining_hrs = (remaining_min - (remaining_min % 60)) / 60;

                let remaining_str = if remaining_hrs > 0 {
                    // Translators: the `{}` must be left unmodified, and
                    // it will be replaced by the number of minutes remaining
                    // in the string "N hours M minutes remaining"
                    let minutes = ni18n_f(
                        "{} minute",
                        "{} minutes",
                        remaining_min % 60,
                        &[&(remaining_min % 60).to_string()],
                    );

                    // Translators: `{hours}` and `{minutes}` must be left
                    // unmodified, and they will be replaced by number of
                    // hours and by the translated number of minutes
                    // remaining, respectively
                    ni18n_k(
                        "{hours} hour {minutes} remaining",
                        "{hours} hours {minutes} remaining",
                        remaining_hrs,
                        &[("hours", &remaining_hrs.to_string()), ("minutes", &minutes)],
                    )
                } else {
                    ni18n_f(
                        // Translators: the '{}' must be left unmodified, and
                        // it will be replaced by the number of minutes remaining
                        // in the playlist
                        "{} minute remaining",
                        "{} minutes remaining",
                        remaining_min,
                        &[&remaining_min.to_string()],
                    )
                };

                self.imp()
                    .playlist_view
                    .queue_length_label()
                    .set_label(&remaining_str);
                self.imp()
                    .playlist_view
                    .queue_length_label()
                    .set_visible(true);
            } else {
                self.imp()
                    .playlist_view
                    .queue_length_label()
                    .set_visible(false);
            }
        }
    }

    fn scroll_playlist_to_track(&self) {
        let queue_view = self.imp().playlist_view.queue_view();
        if let Some(player) = self.player()
            && let Some(current_idx) = player.queue().current_track_index()
        {
            debug!("Scrolling playlist to {}", current_idx);
            queue_view
                .upcast_ref::<gtk::Widget>()
                .activate_action("list.scroll-to-item", Some(&current_idx.to_variant()))
                .expect("Failed to activate action");
        }
    }

    fn setup_provider(&self) {
        let imp = self.imp();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &imp.provider, 400);
        }
    }

    fn update_style(&self, track: Option<&Track>) {
        let imp = self.imp();

        if !imp.settings.boolean("enable-recoloring") {
            imp.provider.load_from_string("");
            imp.toast_overlay.remove_css_class("main-window");
            return;
        }

        if let Some(track) = track
            && let Some(bg_colors) = track.cover_palette()
        {
            let mut css = String::new();

            css.push_str(":root {");

            let n_colors = bg_colors.len();
            for (i, color) in bg_colors.iter().enumerate().take(n_colors) {
                let s = format!("--background-color-{}: {};", i, color);
                css.push_str(&s);
            }

            for i in n_colors + 1 - 1..5 {
                css.push_str(&format!(
                    "--background-color-{}: var(--window-bg-color);",
                    i
                ));
            }

            css.push('}');

            imp.provider.load_from_string(&css);
            if !imp.toast_overlay.has_css_class("main-window") {
                imp.toast_overlay.add_css_class("main-window");
            }

            self.action_set_enabled("win.enable-recoloring", true);

            return;
        }

        imp.provider.load_from_string("");
        imp.toast_overlay.remove_css_class("main-window");
        self.action_set_enabled("win.enable-recoloring", false);
    }

    fn update_title(&self, track: Option<&Track>) {
        if let Some(track) = track {
            self.set_title(Some(&format!("{} - {}", track.artist(), track.title())));
        } else {
            self.set_title(Some("Amberol"));
        }
    }

    fn update_selected_count(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            let n_selected = queue.n_selected_tracks();

            let selected_str = if n_selected == 0 {
                i18n("No track selected")
            } else {
                ni18n_f(
                    // Translators: The '{}' must be left unmodified, and
                    // it is expanded to the number of tracks selected
                    "{} track selected",
                    "{} tracks selected",
                    n_selected,
                    &[&n_selected.to_string()],
                )
            };

            self.imp()
                .playlist_view
                .queue_selected_label()
                .set_label(&selected_str);
        }
    }

    pub fn open_files(&self, files: &[gio::File]) {
        if files.is_empty() {
            self.add_toast(i18n("Unable to access files"));
            self.end_startup();
            return;
        }

        let model = gio::ListStore::new::<gio::File>();
        for f in files {
            model.append(f);
        }
        self.add_files_to_queue(model.upcast_ref::<gio::ListModel>());
    }

    // Selects all the tracks in the playlist, or the ones matching the
    // search
    fn select_all_playlist_tracks(&self) {
        self.set_playlist_selection(true);
        if self.playlist_search() {
            if let Some(ref model) = *self.imp().playlist_filtermodel.borrow() {
                for idx in 0..model.n_items() {
                    if let Some(track) = model.item(idx).and_downcast::<Track>() {
                        track.set_selected(true);
                    }
                }
            }
        } else if let Some(player) = self.player() {
            let queue = player.queue();
            for idx in 0..queue.n_tracks() {
                queue.track_at(idx).unwrap().set_selected(true);
            }
        }
        self.update_selected_count();
    }

    // Selects or unselects a track of the playlist, entering selection
    // mode if needed
    fn toggle_playlist_track(&self, track: &Track) {
        if !self.playlist_selection() {
            self.set_playlist_selection(true);
            track.set_selected(true);
        } else {
            track.set_selected(!track.selected());
        }
        self.update_selected_count();
    }

    fn remove_selected_tracks(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            // Collect all tracks to be removed first, since we can't
            // remove objects from the model while we're iterating it
            let remove_tracks: Vec<Track> = (0..queue.n_tracks())
                .filter_map(|idx| queue.track_at(idx))
                .filter(|track| track.selected())
                .collect();

            for track in remove_tracks {
                self.remove_track(&track);
            }

            // Store the current state of the playlist
            utils::store_playlist(queue);
        }
    }

    // Leaves selection mode in the grid, the tracks of an album and the
    // playlist; returns whether any of them was in it
    fn exit_selection_mode(&self) -> bool {
        let imp = self.imp();
        let mut exited = false;
        if imp.library_view.selection_mode() {
            imp.library_view.set_selection_mode(false);
            exited = true;
        }
        if imp.album_tracks.selection_mode() {
            imp.album_tracks.set_selection_mode(false);
            exited = true;
        }
        if self.playlist_selection() {
            self.set_playlist_selection(false);
            exited = true;
        }
        exited
    }

    // Ctrl+A selects all the items of the list with the focus: the album
    // grid, the tracks of an album, or the playlist
    fn select_all(&self) {
        let imp = self.imp();
        let focus = gtk::prelude::GtkWindowExt::focus(self);
        let focus_in = |widget: &gtk::Widget| focus.as_ref().is_some_and(|f| f.is_ancestor(widget));
        let sidebar_page = imp.sidebar_stack.visible_child_name();

        if self.library_visible() && focus_in(imp.library_view.upcast_ref()) {
            imp.library_view.select_all();
        } else if focus_in(imp.album_tracks.upcast_ref()) {
            imp.album_tracks.select_all();
        } else if self.playlist_visible() && sidebar_page.as_deref() == Some("playlist") {
            self.select_all_playlist_tracks();
        } else if self.playlist_visible() && sidebar_page.as_deref() == Some("album") {
            imp.album_tracks.select_all();
        } else if self.library_visible() {
            imp.library_view.select_all();
        }
    }

    pub fn remove_track(&self, track: &Track) {
        if let Some(p) = self.player() {
            p.remove_track(track);
        }
        self.update_selected_count();
        self.update_playlist_time();
    }

    pub fn add_toast(&self, msg: String) {
        let toast = adw::Toast::new(&msg);
        self.imp().toast_overlay.add_toast(toast);
    }

    pub fn add_skip_to_toast(&self, msg: String, button: String, pos: u32) {
        let toast = adw::Toast::new(&msg);
        toast.set_button_label(Some(&button));
        toast.set_action_name(Some("win.skip-to"));
        toast.set_action_target_value(Some(&pos.to_variant()));
        self.imp().toast_overlay.add_toast(toast);
    }

    fn copy_track(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            if let Some(track) = state.track() {
                let s = i18n_k(
                    // Translators: `{title}` and `{artist}` must be left
                    // untranslated; they will expand to the title and
                    // artist of the currently playing track, respectively
                    "Currently playing “{title}” by “{artist}”",
                    &[("title", &track.title()), ("artist", &track.artist())],
                );
                self.clipboard().set_text(&s);
            }
        }
    }

    fn show_track_info(&self) {
        if let Some(player) = self.player()
            && let Some(track) = player.state().track()
        {
            self.show_track_info_for(&track);
        }
    }

    fn show_track_info_for(&self, track: &Track) {
        let dialog = TrackInfoDialog::new(track);

        // Follow the colors of the cover, like the main window
        if self.imp().toast_overlay.has_css_class("main-window") {
            dialog.add_css_class("recolored");
        }

        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    async fn open_track_directory(&self) {
        if let Some(player) = self.player() {
            let state = player.state();

            if let Some(track) = state.track() {
                let file = track.file();

                match file
                    .query_info_future(
                        ATTRIBUTE_HOST_PATH,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    Err(err) => debug!("Unable to get host-path attribute: {err}"),
                    Ok(info) => {
                        let host_path = info
                            .attribute_as_string(ATTRIBUTE_HOST_PATH)
                            .and_then(|x| PathBuf::from_str(x.as_str()).ok());

                        if let Some(path) = host_path.or_else(|| file.path()) {
                            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
                            if let Err(_e) = launcher
                                .open_containing_folder_future(Some(
                                    self.upcast_ref::<gtk::Window>(),
                                ))
                                .await
                            {
                                self.add_toast(i18n("Unable to open folder"))
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn set_track_time(&self, elapsed: Option<u64>, remaining: Option<u64>) {
        if let Some(elapsed) = elapsed {
            self.imp()
                .elapsed_label
                .set_text(&utils::format_time(elapsed as i64));
        } else {
            self.imp().elapsed_label.set_text("0:00");
        }

        if let Some(remaining) = remaining {
            self.imp()
                .remaining_label
                .set_text(&utils::format_remaining_time(remaining as i64));
        } else {
            self.imp().remaining_label.set_text("0:00");
        }
    }

    pub fn set_track_position(&self, position: f64) {
        self.imp().waveform_view.set_position(position);
    }
}
