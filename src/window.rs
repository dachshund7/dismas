// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod albums;
mod bindings;
mod favorites;
mod fullscreen;
mod loading;
mod now_playing;
mod playlist;
mod selected_playlist;
mod selection;
mod sidebar;
mod sleep_timer;
mod smart_playlists;
mod track_menu;
mod zen;

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    time::Instant,
};

use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, gdk, gio, glib, graphene, prelude::*};
use log::debug;
use selected_playlist::ReplacedQueue;
use sidebar::SizeLimits;

use crate::{
    album_tracks::AlbumTracks,
    audio::{AudioPlayer, CoverCache, Missing, RepeatMode, ReplayGainMode, Track, load_tracks},
    config::APPLICATION_ID,
    cover_backdrop::CoverBackdrop,
    drag_overlay::DragOverlay,
    i18n::{i18n, i18n_k, ni18n_f, ni18n_k},
    library::{self, Album},
    library_view::{AlbumDrag, LibraryView},
    page_switcher::PageSwitcher,
    play_stats,
    playback_control::PlaybackControl,
    player_box::PlayerBox,
    playlist_view::PlaylistView,
    playlists::{self, NameError},
    playlists_page::PlaylistsPage,
    preferences_dialog::PreferencesDialog,
    push_bin::PushBin,
    queue_row::QueueRow,
    resume_point::{ResumePoint, ResumePoints},
    search::SearchFilter,
    slide_bin::SlideBin,
    track_cover::TrackCover,
    track_details::TrackDetails,
    track_info_dialog::TrackInfoDialog,
    utils,
    waveform_view::WaveformView,
    work_sections::{WorkSectionModel, work_header_factory},
};

// Something that waits for the end of startup, see `after_startup()`
type AfterStartup = Box<dyn FnOnce(&Window)>;

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
    // Leave the page and the pane as they are, for tracks added from the
    // command line while the user may be doing something else
    Quiet,
}

// What else happens to the tracks `load_into_queue()` reads
#[derive(Default)]
struct LoadOptions {
    // They take the place of the tracks of the selected playlist, as those
    // of the playlist named. Only this load does that, as files can be
    // dropped meanwhile, and their tracks go to the playlist that is
    // selected when they're read
    switch_to: Option<String>,
    // Playback is paused where it was, in the empty queue
    resume: Option<ResumePoint>,
    // The tracks took the place of these, which the toast can put back
    replaced: Option<ReplacedQueue>,
    // They go before this track, or at the end if it was removed meanwhile
    insert_before: Option<Track>,
}

mod imp {
    use adw::prelude::AdwApplicationWindowExt;

    use super::*;

    #[derive(CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::Window)]
    #[template(resource = "/io/github/dachshund7/Dismas/window.ui")]
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
        pub seek_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub elapsed_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub remaining_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub playback_control: TemplateChild<PlaybackControl>,
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub player_header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub player_backdrop: TemplateChild<CoverBackdrop>,
        #[template_child]
        pub main_box: TemplateChild<PlayerBox>,
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
        pub playlist_pane: TemplateChild<gtk::Box>,
        #[template_child]
        pub sidebar_handle: TemplateChild<gtk::Box>,
        #[template_child]
        pub narrow_breakpoint: TemplateChild<adw::Breakpoint>,

        pub provider: gtk::CssProvider,
        // The colors of the cover the provider has, as CSS
        pub provider_css: RefCell<String>,
        pub settings: gio::Settings,

        #[property(get, set = Self::set_queue_shuffled, explicit_notify)]
        pub queue_shuffled: Cell<bool>,
        // Whether the playing track is a favorite, for the heart button
        #[property(get, set = Self::set_track_favorite, explicit_notify)]
        pub track_favorite: Cell<bool>,
        #[property(get, set = Self::set_playlist_visible, explicit_notify)]
        pub playlist_visible: Cell<bool>,
        // The same, as the user shows and hides the pane, with the pane
        // button, F9 and Esc, which is kept for the next start; the pane
        // also opens by itself, e.g. to show the tracks added
        #[property(name = "user-playlist-visible", get = Self::user_playlist_visible, set = Self::set_user_playlist_visible, explicit_notify, type = bool)]
        user_playlist_visible: std::marker::PhantomData<bool>,
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
        // The sizes the window is limited to, from measuring its content
        pub size_limits: Cell<Option<SizeLimits>>,
        pub size_limits_idle: Cell<bool>,
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
        // Whether the music folders are being read, for the page switchers;
        // the one in the album grid's header is in the window before the
        // library view is bound, so it can't follow the view itself
        #[property(get, set)]
        pub library_scanning: Cell<bool>,
        // Whether the queue might still be restored or opened at startup
        pub starting: Cell<bool>,
        // What waits for the end of startup, e.g. files from the command line
        pub after_startup: RefCell<Vec<AfterStartup>>,
        // Whether to hide the focus again once the page is shown
        pub hide_focus_after_transition: Cell<bool>,
        // The playlist whose tracks are loading, to take the place of
        // those of the selected one once they're loaded
        pub pending_playlist: RefCell<Option<String>>,
        // Scrolling the queue to the playing track, once it's laid out
        pub queue_scroll_tick: RefCell<Option<gtk::TickCallbackId>>,
        // The length of the tracks after the current one, in seconds, so
        // that each position only adds what's left of the current track
        pub queue_time_after_current: Cell<u64>,
        pub queue_time_idle: Cell<bool>,
        // The number of times tracks are being read for the queue
        pub loads: Cell<u32>,
        // Whether the sidebar was showing before the tracks of an album
        pub sidebar_was_visible: Cell<bool>,
        // Whether the sidebar was showing before the split view collapsed
        pub sidebar_visible_uncollapsed: Cell<bool>,

        // Zen mode: the wait before the controls fade out, and their fading
        pub zen_timeout: RefCell<Option<glib::SourceId>>,
        pub zen_animation: RefCell<Option<adw::TimedAnimation>>,
        // Where the pointer last moved to, as only moving it counts in
        // fullscreen
        pub zen_pointer: Cell<Option<(f64, f64)>>,

        // Fullscreen: the cover the backdrop is blurred from, and the page
        // and the pane to go back to
        pub backdrop_source: RefCell<Option<gdk::Texture>>,
        // The cover at full size, with the id of the album's cover, which
        // the next tracks of the album show without decoding it again
        pub full_cover: RefCell<Option<(String, gdk::Texture)>>,
        pub page_before_fullscreen: RefCell<Option<String>>,
        pub pane_before_fullscreen: Cell<bool>,

        // The search of the queue, and the tracks it lets through
        pub queue_filter: SearchFilter,
        pub queue_filtermodel: RefCell<Option<gio::ListModel>>,
        // The rows of the queue, to slide them to their new places when
        // tracks are moved
        pub queue_rows: RefCell<Vec<glib::WeakRef<SlideBin>>>,
        // The row showing where files dragged over the pane would go
        pub file_drop_row: glib::WeakRef<QueueRow>,
        // The header of the work at the top of the queue, when files would
        // go above it
        pub file_drop_header: glib::WeakRef<gtk::Widget>,
        // The pane was shown for dragging albums, and is hidden again if
        // they aren't dropped
        pub pane_opened_for_drag: Cell<bool>,

        // The application's player, see `Window::player()`
        pub player: std::cell::OnceCell<Rc<AudioPlayer>>,

        // The toast about the last track that failed to play, which the
        // next one takes the place of
        pub failed_toast: glib::WeakRef<adw::Toast>,

        // The handlers connected to objects that outlive the window, the
        // player's and the style manager, which are disconnected when it
        // closes: one reaching the closed window would find its widgets gone
        pub handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "DismasWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            PageSwitcher::static_type();
            Self::bind_template(klass);

            klass.install_action("win.play", None, move |win, _, _| {
                debug!("Window::win.play()");
                win.player().toggle_play();
            });
            klass.install_action("win.seek-backwards", None, move |win, _, _| {
                debug!("Window::win.seek-backwards");
                win.player().seek_backwards();
            });
            klass.install_action("win.seek-forward", None, move |win, _, _| {
                debug!("Window::win.seek-forward");
                win.player().seek_forward();
            });
            klass.install_action("win.volume-up", None, move |win, _, _| {
                debug!("Window::win.volume-up()");
                win.imp().playback_control.volume_control().step(1.0);
            });
            klass.install_action("win.volume-down", None, move |win, _, _| {
                debug!("Window::win.volume-down()");
                win.imp().playback_control.volume_control().step(-1.0);
            });
            klass.install_action("win.previous", None, move |win, _, _| {
                debug!("Window::win.previous()");
                win.player().skip_previous();
            });
            klass.install_action("win.next", None, move |win, _, _| {
                debug!("Window::win.next()");
                win.player().skip_next();
            });
            klass.install_action("queue.repeat-mode", None, move |win, _, _| {
                debug!("Window::queue.repeat()");
                win.player().toggle_repeat_mode();
            });
            klass.install_action("win.copy", None, move |win, _, _| {
                debug!("Window::win.copy()");
                win.copy_track();
            });
            klass.install_action("win.track-info", None, move |win, _, _| {
                debug!("Window::win.track-info()");
                win.show_track_info();
            });
            klass.install_action("win.preferences", None, move |win, _, _| {
                debug!("Window::win.preferences()");
                win.show_preferences();
            });
            klass.install_action_async("win.open-directory", None, async move |win, _, _| {
                debug!("Window::win.open-directory()");
                win.open_playing_track_directory().await;
            });
            klass.install_action("win.show-playing-track", None, move |win, _, _| {
                debug!("Window::win.show-playing-track()");
                win.show_playing_track();
            });
            klass.install_action("win.go-to-album", None, move |win, _, _| {
                debug!("Window::win.go-to-album()");
                win.go_to_playing_album();
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
                "win.play-smart-playlist",
                Some(glib::VariantTy::STRING),
                move |win, _, param| {
                    debug!("Window::win.play-smart-playlist()");
                    if let Some(name) = param.and_then(String::from_variant) {
                        win.set_playlists_visible(false);
                        win.play_smart_playlist(&name);
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
            klass.install_action("win.save-queue", None, move |win, _, _| {
                debug!("Window::win.save-queue()");
                win.show_save_queue_dialog();
            });
            // The search button toggles the queue search, but its shortcut
            // goes back to it when it's open; Esc closes it
            klass.install_action("queue.find", None, move |win, _, _| {
                debug!("Window::queue.find()");
                win.set_queue_search(true);
                win.imp().playlist_view.queue_searchentry().grab_focus();
            });
            klass.install_action("queue.play-results", None, move |win, _, _| {
                debug!("Window::queue.play-results()");
                win.play_search_results();
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

            // Esc leaves selection mode, or else hides the playlist pane, or
            // else leaves fullscreen; the collapsed split view hides its
            // pane on Esc by itself, so the pane only needs hiding here when
            // it's beside the content
            klass.add_binding(gdk::Key::Escape, gdk::ModifierType::empty(), |win| {
                if win.exit_selection_mode() {
                    return glib::Propagation::Stop;
                }
                if win.playlist_visible() && !win.imp().split_view.is_collapsed() {
                    win.set_user_playlist_visible(false);
                    return glib::Propagation::Stop;
                }
                if win.is_fullscreen() {
                    win.unfullscreen();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });

            // GtkWindow's own property, which also follows the window
            // manager's fullscreen
            klass.install_property_action("win.fullscreen", "fullscreened");
            klass.install_property_action("queue.toggle", "user-playlist-visible");
            klass.install_property_action("queue.shuffle", "queue-shuffled");
            klass.install_property_action("win.favorite", "track-favorite");
            klass.install_property_action("queue.select", "queue-selection");
            klass.install_property_action("queue.search", "queue-search");
            klass.install_property_action("win.page", "page");
            klass.install_property_action("win.replaygain", "replaygain-mode");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            CoverBackdrop::static_type();
            PlayerBox::static_type();
            PushBin::static_type();
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
                player_header_bar: TemplateChild::default(),
                player_backdrop: TemplateChild::default(),
                main_box: TemplateChild::default(),
                waveform_view: TemplateChild::default(),
                seek_scale: TemplateChild::default(),
                elapsed_label: TemplateChild::default(),
                remaining_label: TemplateChild::default(),
                playlist_view: TemplateChild::default(),
                content_stack: TemplateChild::default(),
                library_view: TemplateChild::default(),
                playlists_page: TemplateChild::default(),
                sidebar_stack: TemplateChild::default(),
                album_tracks: TemplateChild::default(),
                playlist_pane: TemplateChild::default(),
                sidebar_handle: TemplateChild::default(),
                narrow_breakpoint: TemplateChild::default(),
                queue_shuffled: Cell::new(false),
                track_favorite: Cell::new(false),
                playlist_visible: Cell::new(true),
                user_playlist_visible: std::marker::PhantomData,
                queue_selection: Cell::new(false),
                queue_anchor: glib::WeakRef::new(),
                queue_search: Cell::new(false),
                queue_filter: SearchFilter::new(),
                queue_filtermodel: RefCell::default(),
                queue_rows: RefCell::default(),
                file_drop_row: glib::WeakRef::new(),
                file_drop_header: glib::WeakRef::new(),
                pane_opened_for_drag: Cell::new(false),
                replaygain_mode: Cell::new(ReplayGainMode::default()),
                sidebar_width: Cell::new(0),
                size_limits: Cell::new(None),
                size_limits_idle: Cell::new(false),
                library_visible: Cell::new(false),
                playlists_visible: Cell::new(false),
                page: std::marker::PhantomData,
                player_available: Cell::new(false),
                library_scanning: Cell::new(false),
                starting: Cell::new(true),
                after_startup: RefCell::default(),
                pending_playlist: RefCell::default(),
                queue_scroll_tick: RefCell::default(),
                queue_time_after_current: Cell::new(0),
                queue_time_idle: Cell::new(false),
                loads: Cell::new(0),
                hide_focus_after_transition: Cell::new(false),
                sidebar_was_visible: Cell::new(false),
                sidebar_visible_uncollapsed: Cell::new(true),
                zen_timeout: RefCell::default(),
                zen_animation: RefCell::default(),
                zen_pointer: Cell::new(None),
                backdrop_source: RefCell::default(),
                full_cover: RefCell::default(),
                page_before_fullscreen: RefCell::default(),
                pane_before_fullscreen: Cell::new(false),
                provider: gtk::CssProvider::new(),
                provider_css: RefCell::default(),
                settings: utils::settings_manager(),
                player: std::cell::OnceCell::new(),
                failed_toast: glib::WeakRef::new(),
                handlers: RefCell::default(),
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

            self.library_view
                .bind_property("scanning", &*self.obj(), "library-scanning")
                .sync_create()
                .build();

            // Only in selection mode
            self.obj()
                .action_set_enabled("queue.remove-selected", false);

            // Dragging the header bars moves the window, like in other
            // applications, and on Wayland it's how the window is moved;
            // the setting turns it off for window managers that move windows
            // by themselves, like tiling ones. It applies to the header bars
            // of the dialogs too, which move the window as well. The content
            // of the window doesn't move it either way. Switched in the
            // preferences, it applies right away, to their dialog too
            self.settings.connect_changed(
                Some("move-from-header-bars"),
                clone!(
                    #[weak(rename_to = win)]
                    self.obj(),
                    move |settings, key| {
                        let enabled = settings.boolean(key);
                        utils::set_window_drag(win.upcast_ref(), enabled);
                        if let Some(dialog) = win.visible_dialog() {
                            utils::set_window_drag(dialog.upcast_ref(), enabled);
                        }
                    }
                ),
            );
            if !self.settings.boolean("move-from-header-bars") {
                utils::set_window_drag(self.obj().upcast_ref(), false);
            }
            self.obj().connect_visible_dialog_notify(|win| {
                if let Some(dialog) = win.visible_dialog()
                    && !win.imp().settings.boolean("move-from-header-bars")
                {
                    utils::set_window_drag(dialog.upcast_ref(), false);
                }
            });

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
            utils::setup_primary_menu_button(&self.menu_button);
        }
    }

    impl Window {
        fn set_playlist_visible(&self, visible: bool) {
            if visible != self.playlist_visible.replace(visible) {
                self.split_view.set_show_sidebar(visible);
                self.obj().notify_playlist_visible();
                self.obj().notify_user_playlist_visible();
            }
        }

        fn user_playlist_visible(&self) -> bool {
            self.playlist_visible.get()
        }

        // In the collapsed split view, the pane covers the player for a
        // while, and what's kept is whether it shows once the window is
        // wide again
        fn set_user_playlist_visible(&self, visible: bool) {
            let obj = self.obj();
            obj.set_playlist_visible(visible);
            if !self.split_view.is_collapsed()
                && self.settings.boolean("playlist-visible") != visible
                && let Err(e) = self.settings.set_boolean("playlist-visible", visible)
            {
                debug!("Unable to store playlist-visible: {e}");
            }
        }

        // Asked for with the shuffle button or its shortcut: shuffling keeps
        // the order from before, and turning it off puts it back. The
        // button shows whether the selected playlist has one, so it's only
        // set from its file otherwise, with `sync_queue_shuffled()`
        fn set_queue_shuffled(&self, shuffled: bool) {
            let obj = self.obj();
            if shuffled == self.queue_shuffled.get() {
                return;
            }
            if shuffled {
                obj.shuffle_queue();
            } else {
                obj.unshuffle_queue();
            }
            obj.sync_queue_shuffled();
        }

        // Asked for with the heart button or its shortcut; it follows the
        // playing track with `sync_track_favorite()`
        fn set_track_favorite(&self, favorite: bool) {
            let obj = self.obj();
            if favorite == self.track_favorite.get() {
                return;
            }
            let Some(track) = obj.player().state().track() else {
                return;
            };
            play_stats::set_favorite(&track.uri(), favorite);
            self.track_favorite.set(favorite);
            obj.notify_track_favorite();
        }

        fn set_queue_selection(&self, selection: bool) {
            let obj = self.obj();

            if selection != self.queue_selection.replace(selection) {
                if !selection {
                    obj.player().queue().unselect_all_tracks();
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
            // Typing searches the albums right away, and asking for the
            // albums again goes back to their search
            if page == "library" {
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
                obj.player().set_replaygain(replaygain);
                if let Err(e) = self.settings.set_enum("replay-gain", replaygain.into()) {
                    log::warn!("Unable to store the ReplayGain mode: {e}");
                }

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

            // The content is measured again here, where its style is up
            // to date, in case the font changed
            self.obj().queue_size_limits_update();
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
    pub fn new(application: &crate::application::Application) -> Self {
        let win = glib::Object::builder::<Window>()
            .property("application", application)
            .build();
        // Before anything uses it
        win.imp().player.set(application.player()).unwrap();

        win.setup_waveform();
        win.setup_actions();
        win.setup_sleep_timer();
        win.setup_playlist();
        win.setup_sidebar_handle();
        win.setup_zen_mode();
        win.setup_fullscreen();
        win.setup_drop_target();
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        win.setup_x11_drop_shape();
        win.setup_select_all_shortcut();
        win.setup_key_shortcuts();
        win.setup_track_info_button();
        win.setup_more_button();
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

    // Saves where playback is in the selected playlist, right away
    fn save_resume_point(&self) {
        if let Some(app) = self.application() {
            app.downcast::<crate::application::Application>()
                .unwrap()
                .save_resume_point();
        }
    }

    fn plays_in_background(&self) -> bool {
        self.application()
            .and_downcast::<crate::application::Application>()
            .is_some_and(|app| app.plays_in_background())
    }

    // The application's player, which the window keeps from when it's
    // made: the window lives as long as the application, as closing it
    // either hides it or quits
    fn player(&self) -> Rc<AudioPlayer> {
        self.imp()
            .player
            .get()
            .expect("the window has the player from when it's made")
            .clone()
    }

    fn setup_actions(&self) {
        // The seek bar takes the place of the waveform; the setting is only
        // read, never written, by the widgets
        self.imp()
            .settings
            .bind("show-waveform", &*self.imp().waveform_view, "visible")
            .get()
            .build();
        self.imp()
            .settings
            .bind("show-waveform", &*self.imp().seek_scale, "visible")
            .get()
            .invert_boolean()
            .build();

        // Like `win.select-playlist`, with the selected playlist as its
        // state, for the menu of the playlist pane's title to mark it
        let selected = gio::SimpleAction::new_stateful(
            "selected-playlist",
            Some(glib::VariantTy::STRING),
            &self.selected_playlist().to_variant(),
        );
        selected.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, param| {
                debug!("Window::win.selected-playlist()");
                // Unlike the actions installed on the class, this one can be
                // activated over D-Bus, by other programs too
                if let Some(name) = param.and_then(String::from_variant)
                    && (name.is_empty() || playlists::exists(&name))
                {
                    win.set_playlists_visible(false);
                    win.select_playlist(&name);
                }
            }
        ));
        self.add_action(&selected);

        // In the main menu of every page, as new music can be added while
        // the player is showing; it's off while the folder is being read
        let rescan = gio::SimpleAction::new("rescan-library", None);
        rescan.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.imp().library_view.start_scan(true)
        ));
        self.imp()
            .library_view
            .bind_property("scanning", &rescan, "enabled")
            .invert_boolean()
            .sync_create()
            .build();
        self.add_action(&rescan);
    }

    // Space toggles playback wherever the focus is, so it's caught before
    // the focused widget, which would take it to activate itself; text
    // entries keep it, to type spaces. Dialogs keep it too, as the window's
    // shortcuts don't run while one is open.
    //
    // Page Up, Page Down, Home and End scroll a list, see `scroll_list()`;
    // the lists would move their focus instead. Text entries keep Home and
    // End when they have text, to move the cursor; the album search has the
    // focus while the grid shows.
    //
    // Ctrl+Left and Ctrl+Right seek, here rather than as accels, which would
    // run before a text entry with text could move its cursor by word; the
    // lists and the grid would move their focus with them.
    //
    // The releases are kept from the window too, which would show the focus
    // ring for the key, as if it had moved the focus; and holding Space
    // down toggles playback once, not with every repeat
    fn setup_key_shortcuts(&self) {
        let controller = gtk::EventControllerLegacy::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pressed = Cell::new(None);
        controller.connect_event(move |controller, event| {
            let Some(key) = event.downcast_ref::<gdk::KeyEvent>() else {
                return glib::Propagation::Proceed;
            };
            let keyval = key.keyval();

            // Before looking at the modifiers, as a key handled here can be
            // released while one is held
            if event.event_type() == gdk::EventType::KeyRelease {
                return if pressed.get() == Some(keyval) {
                    pressed.set(None);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                };
            }

            let ctrl = match key.modifier_state() & gtk::accelerator_get_default_mod_mask() {
                gdk::ModifierType::CONTROL_MASK => true,
                modifiers if modifiers.is_empty() => false,
                _ => return glib::Propagation::Proceed,
            };

            let win = controller.widget().and_downcast::<Window>().unwrap();
            let focus = gtk::prelude::GtkWindowExt::focus(&win);
            let entry = win.focused_editable();
            let in_entry = entry.is_some();
            let in_text = entry.is_some_and(|entry| !entry.text().is_empty());
            let handled = match keyval {
                gdk::Key::Left | gdk::Key::KP_Left if ctrl && !in_text => {
                    WidgetExt::activate_action(&win, "win.seek-backwards", None).is_ok()
                }
                gdk::Key::Right | gdk::Key::KP_Right if ctrl && !in_text => {
                    WidgetExt::activate_action(&win, "win.seek-forward", None).is_ok()
                }
                _ if ctrl => false,
                gdk::Key::space if pressed.get() == Some(keyval) => true,
                gdk::Key::space => {
                    !in_entry && WidgetExt::activate_action(&win, "win.play", None).is_ok()
                }
                gdk::Key::Page_Up | gdk::Key::KP_Page_Up => {
                    win.scroll_list(focus, gtk::ScrollType::PageBackward);
                    true
                }
                gdk::Key::Page_Down | gdk::Key::KP_Page_Down => {
                    win.scroll_list(focus, gtk::ScrollType::PageForward);
                    true
                }
                gdk::Key::Home | gdk::Key::KP_Home if !in_text => {
                    win.scroll_list(focus, gtk::ScrollType::Start);
                    true
                }
                gdk::Key::End | gdk::Key::KP_End if !in_text => {
                    win.scroll_list(focus, gtk::ScrollType::End);
                    true
                }
                _ => false,
            };
            if handled {
                pressed.set(Some(keyval));
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.add_controller(controller);
    }

    // Scrolls a list like its scrollbar would: the list of the pane under
    // the pointer, e.g. the album grid below the album search, and none
    // over the player. Without the pointer over the window, the list of the
    // pane with the focus, or else the playlist, if it's showing. The key
    // is used up even without a list, as the focused list would scroll
    // itself
    fn scroll_list(&self, focus: Option<gtk::Widget>, scroll: gtk::ScrollType) {
        let scrolled = match self.pointer_widget() {
            Some(widget) => pane_list(&widget),
            None => focus
                .and_then(|widget| pane_list(&widget))
                .or_else(|| Some(self.imp().playlist_view.queue_scrolled())),
        };
        if let Some(scrolled) = scrolled
            && scrolled.is_drawable()
        {
            scrolled.emit_scroll_child(scroll, false);
        }
    }

    // The text entry with the focus: the focus is on the entry's text, or on
    // the entry itself
    fn focused_editable(&self) -> Option<gtk::Editable> {
        gtk::prelude::GtkWindowExt::focus(self)?
            .ancestor(gtk::Editable::static_type())
            .and_downcast()
    }

    // The widget under the pointer, if it's over the window
    fn pointer_widget(&self) -> Option<gtk::Widget> {
        let surface = self.surface()?;
        let pointer = WidgetExt::display(self).default_seat()?.pointer()?;
        let (x, y, _) = surface.device_position(&pointer)?;
        let (dx, dy) = self.surface_transform();
        self.pick(x - dx, y - dy, gtk::PickFlags::DEFAULT)
    }

    fn restore_window_state(&self) {
        let settings = utils::settings_manager();
        let width = settings.int("window-width");
        let height = settings.int("window-height");
        self.set_default_size(width, height);
        self.set_playlist_visible(settings.boolean("playlist-visible"));
    }

    // When the window is closed, and when quitting, which doesn't close it;
    // the playlist pane is stored when the user shows or hides it. The keys
    // can be locked down by the system administrator
    pub fn save_window_state(&self) {
        debug!("Saving window state");
        let (width, height) = self.default_size();
        let settings = &self.imp().settings;
        if let Err(e) = settings
            .set_int("window-width", width)
            .and_then(|()| settings.set_int("window-height", height))
        {
            log::warn!("Unable to store the window size: {e}");
        }
    }

    // The music folders are read again once the dialog is closed, if they
    // or the folders they leave out changed: a scan can't be stopped, so a
    // folder added by mistake and removed again isn't read in the meantime
    fn show_preferences(&self) {
        let folders = |win: &Self| library::music_folders(&win.imp().settings);
        let before = folders(self);
        let dialog = PreferencesDialog::default();
        adw::prelude::AdwDialogExt::connect_closed(
            &dialog,
            clone!(
                #[weak(rename_to = win)]
                self,
                move |dialog| {
                    if dialog.rescan_requested() || folders(&win) != before {
                        win.imp().library_view.start_scan(false);
                    }
                }
            ),
        );
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    pub fn add_toast(&self, msg: String) {
        self.imp().toast_overlay.add_toast(utils::plain_toast(&msg));
    }

    pub(super) fn add_toast_with_button(
        &self,
        msg: String,
        button: String,
        clicked: impl Fn(&Self) + 'static,
    ) {
        let toast = utils::plain_toast(&msg);
        toast.set_button_label(Some(&button));
        toast.connect_button_clicked(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| clicked(&win)
        ));
        self.imp().toast_overlay.add_toast(toast);
    }

    // The queue can change while the toast is up, so the track is
    // looked up when the button is clicked
    pub fn add_skip_to_toast(&self, msg: String, button: String, track: &Track) {
        let toast = utils::plain_toast(&msg);
        toast.set_button_label(Some(&button));
        toast.connect_button_clicked(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_| {
                let player = win.player();
                if let Some(pos) = player.queue().position(&track) {
                    player.skip_to(pos);
                    player.play();
                }
            }
        ));
        self.imp().toast_overlay.add_toast(toast);
    }
}

// The list of the pane a widget is in; each pane, like the album grid or
// the playlist, is a toolbar view, with its header bars and search bar
// around the list
fn pane_list(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    let pane = widget.ancestor(adw::ToolbarView::static_type())?;
    shown_scrolled_window(&pane)
}

fn shown_scrolled_window(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    if let Some(scrolled) = widget.downcast_ref::<gtk::ScrolledWindow>() {
        return scrolled.is_drawable().then(|| scrolled.clone());
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(scrolled) = shown_scrolled_window(&widget) {
            return Some(scrolled);
        }
        child = widget.next_sibling();
    }
    None
}
