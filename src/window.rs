// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod adding;
mod albums;
mod bindings;
mod booklets;
mod favorites;
mod find;
mod fullscreen;
mod loading;
mod now_playing;
mod pane;
mod playlist;
mod previews;
mod selected_playlist;
mod selection;
mod sidebar;
mod sleep_timer;
mod smart_playlists;
mod track_menu;
mod zen;
mod zoom;

pub(crate) use smart_playlists::SmartPlaylist;

use std::{
    cell::{Cell, RefCell},
    collections::{HashSet, VecDeque},
    rc::Rc,
    time::Instant,
};

use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, gdk, gio, glib, graphene, prelude::*};
use log::debug;
use pane::PaneHistory;
use playlist::QueueUndo;
use selected_playlist::ReplacedQueue;
use sidebar::SizeLimits;

use crate::{
    album_tracks::AlbumTracks,
    audio::{
        AudioPlayer, CoverCache, Missing, RemovedRun, RepeatMode, ReplayGainMode, SortKey, Track,
        load_tracks,
    },
    config::APPLICATION_ID,
    cover_backdrop::CoverBackdrop,
    drag_overlay::DragOverlay,
    i18n::{i18n, i18n_k, ni18n_f, ni18n_k},
    library::{self, Album, TrackFilter},
    library_view::{AlbumDrag, LibraryPage, LibraryView, PickNames},
    lyrics_view::LyricsView,
    page_switcher::PageSwitcher,
    play_stats::{self, FavoriteRecording},
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

// Something that waits: for the end of startup, see `after_startup()`, or
// for the tracks being read for the queue, see `after_loads()`
type Deferred = Box<dyn FnOnce(&Window)>;

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
    // of the playlist named. Files dropped meanwhile wait until they have,
    // see `Window::after_loads()`, and go to the playlist switched to
    switch_to: Option<String>,
    // Playback is paused where it was, in the empty queue
    resume: Option<ResumePoint>,
    // And then goes on from there, as for Play on the tracks of a playlist
    // that isn't selected, which keep the places of the files that aren't
    // there, unlike tracks played with `QueueMode::Play`
    play: bool,
    // The tracks took the place of these, which the toast can put back
    replaced: Option<ReplacedQueue>,
    // Where they go in the queue
    place: Place,
    // What the toast of tracks that replaced those of the queue says,
    // rather than how many there are
    announce: Option<String>,
}

// Where tracks go in the queue, worked out once they're read, as the
// queue can change meanwhile
#[derive(Default)]
enum Place {
    #[default]
    End,
    // Before this track, or at the end if it was removed meanwhile
    Before(Track),
    // To play next, see `Queue::play_next_before()`
    Next,
}

mod imp {
    use adw::prelude::AdwApplicationWindowExt;

    use super::*;

    #[derive(CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::Window)]
    #[template(resource = "/io/github/dachshund7/Dismas/window.ui")]
    pub(crate) struct Window {
        // Template widgets
        #[template_child]
        pub(crate) drag_overlay: TemplateChild<DragOverlay>,
        #[template_child]
        pub(crate) toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub(crate) track_cover: TemplateChild<TrackCover>,
        #[template_child]
        pub(crate) track_details: TemplateChild<TrackDetails>,
        #[template_child]
        pub(crate) waveform_view: TemplateChild<WaveformView>,
        #[template_child]
        pub(crate) seek_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub(crate) elapsed_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) remaining_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) playback_control: TemplateChild<PlaybackControl>,
        #[template_child]
        pub(crate) menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(crate) player_header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub(crate) player_backdrop: TemplateChild<CoverBackdrop>,
        #[template_child]
        pub(crate) main_box: TemplateChild<PlayerBox>,
        #[template_child]
        pub(crate) split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub(crate) playlist_view: TemplateChild<PlaylistView>,
        #[template_child]
        pub(crate) content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(crate) library_view: TemplateChild<LibraryView>,
        #[template_child]
        pub(crate) playlists_page: TemplateChild<PlaylistsPage>,
        #[template_child]
        pub(crate) sidebar_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(crate) album_tracks: TemplateChild<AlbumTracks>,
        #[template_child]
        pub(crate) lyrics_view: TemplateChild<LyricsView>,
        #[template_child]
        pub(crate) playlist_pane: TemplateChild<gtk::Box>,
        #[template_child]
        pub(crate) sidebar_handle: TemplateChild<gtk::Box>,
        #[template_child]
        pub(crate) narrow_breakpoint: TemplateChild<adw::Breakpoint>,

        pub(crate) provider: gtk::CssProvider,
        // The colors of the cover the provider has, as CSS
        pub(crate) provider_css: RefCell<String>,
        pub(crate) settings: gio::Settings,

        #[property(get, set = Self::set_queue_shuffled, explicit_notify)]
        pub(crate) queue_shuffled: Cell<bool>,
        // Whether the playing track is a favorite, for the heart button
        #[property(get, set = Self::set_track_favorite, explicit_notify)]
        pub(crate) track_favorite: Cell<bool>,
        #[property(get, set = Self::set_playlist_visible, explicit_notify)]
        pub(crate) playlist_visible: Cell<bool>,
        // The same, as the user shows and hides the pane, with the pane
        // button, F9 and Esc, which is kept for the next start; the pane
        // also opens by itself, e.g. to show the tracks added
        #[property(name = "user-playlist-visible", get = Self::user_playlist_visible, set = Self::set_user_playlist_visible, explicit_notify, type = bool)]
        user_playlist_visible: std::marker::PhantomData<bool>,
        // Whether the pane shows the lyrics of the playing track
        #[property(name = "lyrics-shown", get = Self::lyrics_shown, set = Self::set_lyrics_shown, explicit_notify, type = bool)]
        lyrics_shown: std::marker::PhantomData<bool>,
        #[property(get, set = Self::set_queue_selection, explicit_notify)]
        pub(crate) queue_selection: Cell<bool>,
        // The last track selected or unselected with a click, where
        // Shift+click starts selecting from
        pub(crate) queue_anchor: glib::WeakRef<Track>,
        #[property(get, set = Self::set_queue_search, explicit_notify)]
        pub(crate) queue_search: Cell<bool>,
        #[property(get, set = Self::set_replaygain_mode, explicit_notify, builder(ReplayGainMode::default()))]
        pub(crate) replaygain_mode: Cell<ReplayGainMode>,
        pub(crate) sidebar_width: Cell<i32>,
        // The sizes the window is limited to, from measuring its content
        pub(crate) size_limits: Cell<Option<SizeLimits>>,
        pub(crate) size_limits_idle: Cell<bool>,
        #[property(get, set = Self::set_library_visible, explicit_notify)]
        pub(crate) library_visible: Cell<bool>,
        // The list of playlists, shown over the player or the album grid
        #[property(get, set = Self::set_playlists_visible, explicit_notify)]
        pub(crate) playlists_visible: Cell<bool>,
        // The visible page: "player", "albums", "works" or "playlists"; the
        // albums and the works are the same page of the content, the
        // library view, whose own page says which
        #[property(name = "page", get = Self::page, set = Self::set_page, explicit_notify, type = String)]
        page: std::marker::PhantomData<String>,
        // The Tracks toggle of the Albums page as shown: off on the other
        // pages, so that Ctrl+T, which toggles it, shows the albums with it
        // on. The library view keeps it while other pages are shown
        #[property(name = "tracks-only", get = Self::tracks_only, set = Self::set_tracks_only, explicit_notify, type = bool)]
        tracks_only: std::marker::PhantomData<bool>,
        // Whether there is anything to play, to go to the player
        #[property(get)]
        pub(crate) player_available: Cell<bool>,
        // Whether the music folders are being read, for the page switchers;
        // the one in the album grid's header is in the window before the
        // library view is bound, so it can't follow the view itself
        #[property(get, set)]
        pub(crate) library_scanning: Cell<bool>,
        // Whether a track is part of a work, for the Works buttons of the
        // page switchers, like `library_scanning`
        #[property(get, set)]
        pub(crate) works_available: Cell<bool>,
        // Whether the queue might still be restored or opened at startup
        pub(crate) starting: Cell<bool>,
        // What waits for the end of startup, e.g. files from the command line
        pub(crate) after_startup: RefCell<Vec<Deferred>>,
        // Whether to hide the focus again once the page is shown
        pub(crate) hide_focus_after_transition: Cell<bool>,
        // The playlist whose tracks are loading, to take the place of
        // those of the selected one once they're loaded
        pub(crate) pending_playlist: RefCell<Option<String>>,
        // The playlist that the tracks page of the pane shows, by its name,
        // empty for the queue, which its Play button switches to; none for
        // an album's tracks or a smart playlist's, which play in place of
        // the queue
        pub(crate) shown_playlist: RefCell<Option<String>>,
        // Bumped each time the tracks page is asked for other tracks, so
        // that a playlist read meanwhile doesn't take their place
        pub(crate) shown_request: Cell<u32>,
        // The playlist being read for the tracks page, if any
        pub(crate) shown_reading: RefCell<Option<String>>,
        // Scrolling the queue to the playing track, once it's laid out
        pub(crate) queue_scroll_tick: RefCell<Option<gtk::TickCallbackId>>,
        // The length of the tracks after the current one, in seconds, so
        // that each position only adds what's left of the current track
        pub(crate) queue_time_after_current: Cell<u64>,
        pub(crate) queue_time_idle: Cell<bool>,
        // The number of times tracks are being read for the queue
        pub(crate) loads: Cell<u32>,
        // What waits for them to be read, in the order asked
        pub(crate) after_loads: RefCell<VecDeque<Deferred>>,
        // What Play a Random … played last on each page, which it doesn't
        // play again right away: the album, by its id, the track, by its
        // URI, and the work, by its key
        pub(crate) last_random_album: RefCell<Option<String>>,
        pub(crate) last_random_track: RefCell<Option<String>>,
        pub(crate) last_random_work: RefCell<Option<String>>,
        // The pages of the pane to go back to, see `PaneHistory`
        pub(crate) pane_history: RefCell<PaneHistory>,
        // Whether the sidebar was showing before the split view collapsed
        pub(crate) sidebar_visible_uncollapsed: Cell<bool>,

        // Zen mode: the wait before the controls fade out, and their fading
        pub(crate) zen_timeout: RefCell<Option<glib::SourceId>>,
        pub(crate) zen_animation: RefCell<Option<adw::TimedAnimation>>,
        // Where the pointer last moved to, as only moving it counts in
        // fullscreen
        pub(crate) zen_pointer: Cell<Option<(f64, f64)>>,

        // Fullscreen: the cover the backdrop is blurred from, and the page
        // and the pane to go back to
        pub(crate) backdrop_source: RefCell<Option<gdk::Texture>>,
        // The cover at full size, with the id of the album's cover, which
        // the next tracks of the album show without decoding it again
        pub(crate) full_cover: RefCell<Option<(String, gdk::Texture)>>,
        pub(crate) page_before_fullscreen: RefCell<Option<String>>,
        pub(crate) pane_before_fullscreen: Cell<bool>,

        // The search of the queue, and the tracks it lets through
        pub(crate) queue_filter: SearchFilter,
        pub(crate) queue_filtermodel: RefCell<Option<gio::ListModel>>,
        // The rows of the queue, to slide them to their new places when
        // tracks are moved
        pub(crate) queue_rows: RefCell<Vec<glib::WeakRef<SlideBin>>>,
        // The row showing where files dragged over the pane would go
        pub(crate) file_drop_row: glib::WeakRef<QueueRow>,
        // The header of the work at the top of the queue, when files would
        // go above it
        pub(crate) file_drop_header: glib::WeakRef<gtk::Widget>,
        // The pane was shown for dragging albums, and is hidden again if
        // they aren't dropped
        pub(crate) pane_opened_for_drag: Cell<bool>,

        // The application's player, see `Window::player()`
        pub(crate) player: std::cell::OnceCell<Rc<AudioPlayer>>,

        // The toast about the last track that failed to play, which the
        // next one takes the place of
        pub(crate) failed_toast: glib::WeakRef<adw::Toast>,
        // The toast with an Undo button that Ctrl+Z presses, while it's up,
        // which the next one takes the place of
        pub(crate) undo_toast: glib::WeakRef<adw::Toast>,
        // What the Undo of the toast about removed tracks puts back, while
        // that toast is up
        pub(super) queue_undo: RefCell<Option<QueueUndo>>,
        // The toast whose Undo puts back the order from before a sort,
        // which goes when the order changes otherwise, see `sort_queue()`
        pub(crate) sort_toast: glib::WeakRef<adw::Toast>,

        // The handlers connected to objects that outlive the window, the
        // player's and the style manager, which are disconnected when it
        // closes: one reaching the closed window would find its widgets gone
        pub(crate) handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
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
            // Installed, rather than added to the window, so that other
            // programs can't activate it over D-Bus: it can't be undone
            klass.install_action("win.clear-play-history", None, move |win, _, _| {
                win.confirm_clear_play_history();
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
            // The volume control's own action, `volume.toggle-mute`, is
            // only found from inside it, where an accel isn't looked for
            klass.install_action("win.toggle-mute", None, move |win, _, _| {
                debug!("Window::win.toggle-mute()");
                let volume = win.imp().playback_control.volume_control();
                volume.set_toggle_mute(!volume.toggle_mute());
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
            // The help on the search syntax, from the main menu and from the
            // line under a search with a problem
            klass.install_action("win.search-syntax", None, move |win, _, _| {
                crate::search_syntax::show(win);
            });
            klass.install_action("win.preferences", None, move |win, _, _| {
                debug!("Window::win.preferences()");
                win.show_preferences();
            });
            // The button of the album page without albums
            klass.install_action("win.choose-music-folders", None, move |win, _, _| {
                debug!("Window::win.choose-music-folders()");
                win.show_preferences().focus_add_folder();
            });
            klass.install_action_async("win.open-directory", None, async move |win, _, _| {
                debug!("Window::win.open-directory()");
                win.open_playing_track_directory().await;
            });
            // The target is the URI of the booklet. The action is the
            // class's, so other programs can't open files with it over
            // D-Bus, as they could the window's own actions
            klass.install_action_async(
                crate::booklets::OPEN_ACTION,
                Some(glib::VariantTy::STRING),
                async move |win, _, target| {
                    debug!("Window::win.open-booklet()");
                    let file = target
                        .as_ref()
                        .and_then(glib::Variant::str)
                        .map(gio::File::for_uri)
                        .filter(|file| file.path().is_some());
                    if let Some(file) = file {
                        win.open_booklet(file).await;
                    }
                },
            );
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
                win.remove_all_tracks();
            });
            klass.install_action(
                "queue.sort",
                Some(glib::VariantTy::STRING),
                move |win, _, param| {
                    debug!("Window::queue.sort()");
                    let key = param
                        .and_then(glib::Variant::str)
                        .and_then(SortKey::from_nick);
                    if let Some(key) = key {
                        win.sort_queue(key);
                    }
                },
            );
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
            // In the album pages' menu, whose item names what the page
            // shows, see `LibraryView::random_pool()`
            klass.install_action("win.play-random", None, move |win, _, _| {
                debug!("Window::win.play-random()");
                win.play_random();
            });
            // The rows of the Playlists page show the tracks of a playlist
            // in the pane, without switching to it, or those a smart
            // playlist picks, without playing them; even while tracks are
            // read, as for an album
            klass.install_action(
                "win.show-playlist",
                Some(glib::VariantTy::STRING),
                move |win, _, param| {
                    debug!("Window::win.show-playlist()");
                    if let Some(name) = param.and_then(String::from_variant) {
                        win.show_playlist_preview(&name);
                    }
                },
            );
            klass.install_action(
                "win.show-smart-playlist",
                Some(glib::VariantTy::STRING),
                move |win, _, param| {
                    debug!("Window::win.show-smart-playlist()");
                    if let Some(kind) = param
                        .and_then(String::from_variant)
                        .and_then(|name| smart_playlists::SmartPlaylist::named(&name))
                    {
                        win.show_smart_playlist_preview(kind);
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
            klass.install_action_async("win.add-tracks", None, async move |win, _, _| {
                debug!("Window::win.add-tracks()");
                win.add_tracks_from_dialog().await;
            });
            klass.install_action_async("win.add-folder", None, async move |win, _, _| {
                debug!("Window::win.add-folder()");
                win.add_folder_from_dialog().await;
            });
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
            // The search button toggles the queue search, but Ctrl+F goes
            // back to it when it's open; Esc closes it. On the album pages,
            // Ctrl+F goes to their search instead, see window/find.rs
            klass.install_action("win.find", None, move |win, _, _| {
                debug!("Window::win.find()");
                win.find();
            });
            // They zoom what the focus is on, see window/zoom.rs
            for (name, zoom) in [
                ("win.zoom-in", crate::zoom::Zoom::In),
                ("win.zoom-out", crate::zoom::Zoom::Out),
                ("win.zoom-reset", crate::zoom::Zoom::Reset),
            ] {
                klass.install_action(name, None, move |win, _, _| {
                    debug!("Window::{name}()");
                    win.zoom(zoom);
                });
            }
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

            klass.add_binding(gdk::Key::Escape, gdk::ModifierType::empty(), |win| {
                if win.escape() {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });

            // GtkWindow's own property, which also follows the window
            // manager's fullscreen
            klass.install_property_action("win.fullscreen", "fullscreened");
            // For the main menu, which would show `win.fullscreen` with a
            // check box; it's hidden in fullscreen
            klass.install_action("win.enter-fullscreen", None, |win, _, _| {
                win.fullscreen();
            });
            klass.install_property_action("queue.toggle", "user-playlist-visible");
            klass.install_property_action("win.lyrics", "lyrics-shown");
            klass.install_property_action("queue.shuffle", "queue-shuffled");
            klass.install_property_action("win.favorite", "track-favorite");
            klass.install_property_action("queue.select", "queue-selection");
            klass.install_property_action("queue.search", "queue-search");
            klass.install_property_action("win.page", "page");
            klass.install_property_action("win.tracks-only", "tracks-only");
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
                lyrics_view: TemplateChild::default(),
                playlist_pane: TemplateChild::default(),
                sidebar_handle: TemplateChild::default(),
                narrow_breakpoint: TemplateChild::default(),
                queue_shuffled: Cell::new(false),
                track_favorite: Cell::new(false),
                playlist_visible: Cell::new(true),
                user_playlist_visible: std::marker::PhantomData,
                lyrics_shown: std::marker::PhantomData,
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
                tracks_only: std::marker::PhantomData,
                player_available: Cell::new(false),
                library_scanning: Cell::new(false),
                works_available: Cell::new(false),
                starting: Cell::new(true),
                after_startup: RefCell::default(),
                pending_playlist: RefCell::default(),
                shown_playlist: RefCell::default(),
                shown_request: Cell::default(),
                shown_reading: RefCell::default(),
                queue_scroll_tick: RefCell::default(),
                queue_time_after_current: Cell::new(0),
                queue_time_idle: Cell::new(false),
                loads: Cell::new(0),
                after_loads: RefCell::default(),
                last_random_album: RefCell::default(),
                last_random_track: RefCell::default(),
                last_random_work: RefCell::default(),
                hide_focus_after_transition: Cell::new(false),
                pane_history: RefCell::default(),
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
                undo_toast: glib::WeakRef::new(),
                queue_undo: RefCell::default(),
                sort_toast: glib::WeakRef::new(),
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
            self.library_view
                .bind_property("has-works", &*self.obj(), "works-available")
                .sync_create()
                .build();
            // The works going away shows the albums
            self.library_view.connect_page_notify(clone!(
                #[weak(rename_to = obj)]
                self.obj(),
                move |_| obj.notify_page()
            ));
            // The toggle as shown follows the page, and the view's toggle
            self.obj()
                .connect_page_notify(super::Window::notify_tracks_only);
            self.library_view.connect_tracks_only_notify(clone!(
                #[weak(rename_to = obj)]
                self.obj(),
                move |_| obj.notify_tracks_only()
            ));

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
            // A dialog keeps its keys from the window's controllers, as a
            // modal window would (libadwaita sets its `limit-events`), so
            // the accels don't run in it: Ctrl+W, which closes the window
            // otherwise, is a shortcut of the dialog that shows, which
            // closes it as Esc does. A dialog over another takes it while
            // it's on top
            let close_dialog = gtk::ShortcutController::new();
            close_dialog.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string("<Control>w"),
                Some(gtk::CallbackAction::new(|widget, _| {
                    if let Some(dialog) = widget.downcast_ref::<adw::Dialog>() {
                        adw::prelude::AdwDialogExt::close(dialog);
                    }
                    glib::Propagation::Stop
                })),
            ));
            self.obj().connect_visible_dialog_notify(move |win| {
                win.update_default_widget();
                if let Some(shown) = close_dialog.widget() {
                    shown.remove_controller(&close_dialog);
                }
                if let Some(dialog) = win.visible_dialog() {
                    dialog.add_controller(close_dialog.clone());
                    if !win.imp().settings.boolean("move-from-header-bars") {
                        utils::set_window_drag(dialog.upcast_ref(), false);
                    }
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
            utils::setup_primary_menu_button(&self.menu_button, None, None);
        }
    }

    impl Window {
        fn set_playlist_visible(&self, visible: bool) {
            if visible != self.playlist_visible.replace(visible) {
                // A playlist still being read would open the pane again
                if !visible {
                    self.obj().request_shown();
                }
                self.split_view.set_show_sidebar(visible);
                // The split view moves the focus out of a pane it hides:
                // beside the content, to the first widget of the page that
                // takes it, and over it, back to where it was before the pane
                // opened; the focus also stays on a button or an album that
                // was clicked. On the album page, it goes to the search
                // instead, so that typing searches and Esc clears the search,
                // unless it's there already, as taking it again would select
                // the text being typed
                if !visible
                    && LibraryPage::named(&self.page()).is_some()
                    && gtk::prelude::GtkWindowExt::focus(&*self.obj())
                        .is_none_or(|focus| !focus.is_ancestor(&self.library_view.search_entry()))
                {
                    self.library_view.focus_search();
                }
                self.obj().notify_playlist_visible();
                self.obj().notify_user_playlist_visible();
                self.obj().notify_lyrics_shown();
            }
        }

        fn user_playlist_visible(&self) -> bool {
            self.playlist_visible.get()
        }

        // GTK reads the states of the property actions while the template
        // is being built, as a widget in it is rooted, before the stack of
        // the pane is bound
        fn lyrics_shown(&self) -> bool {
            self.sidebar_stack.is_bound()
                && self.playlist_visible.get()
                && self.obj().lyrics_on_pane()
        }

        // The lyrics open over the page of the pane, and the pane opens for
        // them, as for the tracks of an album, without keeping it for the
        // next start; if they're on the pane already, the pane was hidden,
        // e.g. with F9, and only shows again. Hiding them goes back to the
        // page from before
        fn set_lyrics_shown(&self, shown: bool) {
            let obj = self.obj();
            if shown == self.lyrics_shown() {
                return;
            }
            if !shown {
                obj.show_pane_back();
            } else if obj.lyrics_on_pane() {
                obj.set_playlist_visible(true);
            } else {
                obj.open_pane_page(pane::PanePage::Lyrics);
            }
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
            // The order the sort's Undo would put back is no longer the
            // one before the last change
            obj.dismiss_sort_toast();
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
            obj.mark_favorite(&track, favorite);
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
                self.library_view.page().name()
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
            let library_page = LibraryPage::named(&page);
            // Without a work, there are no works to go to, as there's no
            // player with nothing to play
            if library_page == Some(LibraryPage::Works) && !self.library_view.has_works() {
                obj.notify_page();
                return;
            }
            // The albums and the works are one page, which shows either
            if let Some(library_page) = library_page {
                self.library_view.set_page(library_page);
            }
            let (library, playlists) = match page.as_str() {
                // Going to the albums or the works from the playlists
                // closes them
                _ if library_page.is_some() => (true, false),
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
            // The music folders that can't be watched are read again
            if library && library_changed {
                self.library_view.scan_unwatched();
            }
            self.update_content_page();
            // Typing searches the albums or the works right away, and asking
            // for them again goes back to their search
            if library_page.is_some() {
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

        fn tracks_only(&self) -> bool {
            self.page() == LibraryPage::Albums.name() && self.library_view.tracks_only()
        }

        // Asked for on another page, the toggle shows the albums with it on
        fn set_tracks_only(&self, tracks_only: bool) {
            let obj = self.obj();
            if tracks_only && self.page() != LibraryPage::Albums.name() {
                obj.set_page(LibraryPage::Albums.name());
            }
            self.library_view.set_tracks_only(tracks_only);
            obj.notify_tracks_only();
        }

        fn update_content_page(&self) {
            let page = self.page();
            let name = if LibraryPage::named(&page).is_some() {
                "library"
            } else {
                &page
            };
            self.content_stack.set_visible_child_name(name);
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
    pub(crate) struct Window(ObjectSubclass<imp::Window>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub(crate) fn new(application: &crate::application::Application) -> Self {
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
        win.setup_edit_shortcuts();
        win.setup_type_to_search();
        win.setup_track_info_button();
        win.setup_more_button();
        win.setup_library();
        win.setup_playlists();
        win.setup_lyrics();
        win.setup_provider();
        win.bind_state();
        win.bind_queue();
        win.connect_signals();
        win.restore_window_state();
        win.set_initial_state();
        win.update_default_widget();
        let play = win.imp().playback_control.play_button();
        play.connect_map(clone!(
            #[weak]
            win,
            move |_| win.update_default_widget()
        ));
        play.connect_unmap(clone!(
            #[weak]
            win,
            move |_| win.update_default_widget()
        ));

        win
    }

    // Enter plays or pauses where the focused widget doesn't take it, as
    // the play button is the default widget. A dialog inside the window
    // passes Enter on to the window, where it played or paused from a row
    // of the preferences, so while one shows the window has no default
    // widget, and Enter activates the row with the focus. Nor while the
    // player page is hidden: GTK drops a default widget that's unmapped,
    // which left Enter to the rows of the other pages, until a dialog had
    // closed and the button was made the default again while hidden
    fn update_default_widget(&self) {
        let play = self.imp().playback_control.play_button();
        let no_dialog = adw::prelude::AdwApplicationWindowExt::visible_dialog(self).is_none();
        self.set_default_widget((no_dialog && play.is_mapped()).then_some(&play));
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
    fn player(&self) -> &Rc<AudioPlayer> {
        self.imp()
            .player
            .get()
            .expect("the window has the player from when it's made")
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

        // Scan Now, in Preferences, for changes the watch on the music
        // folders didn't see; it's off while the folders are being read
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
    // focus while the grid shows. The sliders and the waveform keep them,
    // to go to their ends.
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
            // The text of the lyrics moves its caret with them, as a search
            // with text does, and with Page Up and Page Down too, which only
            // scroll a list; Space still plays and pauses, as the text can't
            // be edited
            let in_text_view = focus.as_ref().is_some_and(ObjectExt::is::<gtk::TextView>);
            let in_text = in_text_view || entry.is_some_and(|entry| !entry.text().is_empty());
            // A slider, or the waveform, takes them to go to its ends
            let in_slider = focus
                .as_ref()
                .is_some_and(|focus| focus.is::<gtk::Range>() || focus.is::<WaveformView>());
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
                gdk::Key::Page_Up | gdk::Key::KP_Page_Up if !in_slider && !in_text_view => {
                    win.scroll_list(focus, gtk::ScrollType::PageBackward);
                    true
                }
                gdk::Key::Page_Down | gdk::Key::KP_Page_Down if !in_slider && !in_text_view => {
                    win.scroll_list(focus, gtk::ScrollType::PageForward);
                    true
                }
                gdk::Key::Home | gdk::Key::KP_Home if !in_text && !in_slider => {
                    win.scroll_list(focus, gtk::ScrollType::Start);
                    true
                }
                gdk::Key::End | gdk::Key::KP_End if !in_text && !in_slider => {
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

    // Ctrl+C copies the details of the track with the focus, or else of the
    // playing track, once the focused widget has had the key: an accel would
    // run before it, and take the key from a search with text selected.
    // Text without a selection turns its own copying off, so the key comes
    // on to the window.
    //
    // Ctrl+Z presses the Undo of a toast, before the focused widget has the
    // key, as GTK's text takes it even with nothing to undo. A search with
    // text keeps it, to undo its typing first
    fn setup_edit_shortcuts(&self) {
        let controller = gtk::ShortcutController::new();
        controller.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>c"),
            Some(gtk::CallbackAction::new(|widget, _| {
                widget
                    .downcast_ref::<Window>()
                    .unwrap()
                    .copy_focused_track();
                glib::Propagation::Stop
            })),
        ));
        self.add_controller(controller);

        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        controller.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>z"),
            Some(gtk::CallbackAction::new(|widget, _| {
                let win = widget.downcast_ref::<Window>().unwrap();
                let in_text = win
                    .focused_editable()
                    .is_some_and(|entry| !entry.text().is_empty());
                if !in_text && win.press_undo() {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            })),
        ));
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

    // Esc leaves selection mode, or else hides the playlist pane, or else
    // leaves fullscreen; the collapsed split view hides its pane on Esc by
    // itself, so the pane only needs hiding here when it's beside the
    // content. Returns whether there was anything to do
    fn escape(&self) -> bool {
        if self.exit_selection_mode() {
            return true;
        }
        if self.playlist_visible() && !self.imp().split_view.is_collapsed() {
            self.set_user_playlist_visible(false);
            return true;
        }
        if self.is_fullscreen() {
            self.unfullscreen();
            return true;
        }
        false
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
    pub(crate) fn save_window_state(&self) {
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
    fn show_preferences(&self) -> PreferencesDialog {
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
        dialog
    }

    pub(crate) fn add_toast(&self, msg: String) {
        self.imp().toast_overlay.add_toast(utils::plain_toast(&msg));
    }

    pub(super) fn add_toast_with_button(
        &self,
        msg: String,
        button: String,
        clicked: impl Fn(&Self) + 'static,
    ) -> adw::Toast {
        let toast = utils::plain_toast(&msg);
        toast.set_button_label(Some(&button));
        toast.connect_button_clicked(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| clicked(&win)
        ));
        self.imp().toast_overlay.add_toast(toast.clone());
        toast
    }

    // A toast whose button undoes something, which Ctrl+Z presses too. It
    // takes the place of the one before: a toast waits behind the one
    // showing, which would then name what came before, and its Undo would
    // put back what was there before that, rather than what Ctrl+Z puts
    // back
    pub(super) fn add_undo_toast(&self, msg: String, undo: impl Fn(&Self) + 'static) -> adw::Toast {
        if let Some(toast) = self.imp().undo_toast.upgrade() {
            toast.dismiss();
        }
        let toast = self.add_toast_with_button(msg, i18n("Undo"), undo);
        toast.connect_dismissed(clone!(
            #[weak(rename_to = win)]
            self,
            move |toast| {
                if win.imp().undo_toast.upgrade().as_ref() == Some(toast) {
                    win.imp().undo_toast.set(None);
                }
            }
        ));
        self.imp().undo_toast.set(Some(&toast));
        toast
    }

    // Ctrl+Z, while a toast with an Undo button is up, presses it: the
    // keyboard can hardly reach the button before the toast goes. Says
    // whether there was one
    fn press_undo(&self) -> bool {
        let Some(toast) = self.imp().undo_toast.upgrade() else {
            return false;
        };
        // A button pressed by hand dismisses its toast, but the signal
        // alone doesn't
        toast.emit_by_name::<()>("button-clicked", &[]);
        toast.dismiss();
        true
    }

    // The queue can change while the toast is up, so the track is
    // looked up when the button is clicked
    pub(crate) fn add_skip_to_toast(&self, msg: String, button: String, track: &Track) {
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
