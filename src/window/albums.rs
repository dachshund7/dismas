// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};

use super::{adding::Tracks, *};
use crate::album_cell::AlbumCell;

impl Window {
    // Shows the tracks of an album in the sidebar, in place of the
    // queue, so that it can be played or added to the queue; with a
    // filter, only those it matches, see `AlbumTracks::show_album()`
    fn show_album_tracks(
        &self,
        album: &Album,
        filter: TrackFilter,
        picks: PickNames,
        reveal: Option<&gio::File>,
    ) {
        let imp = self.imp();
        self.set_shown_playlist(None);
        let hidden = imp.library_view.hidden_folders();
        imp.album_tracks
            .show_album(album, filter, picks, hidden, reveal);
        // Going back shows the page from before, and closes the sidebar if
        // it was opened for them
        self.open_pane_page(pane::PanePage::Tracks);
    }

    // Goes to an album on the Albums page, as Go to Work goes to a work:
    // the grid scrolls to it, clearing a search that hides it, and the
    // whole album opens in the pane. From a track of it, the track flashes
    // there and takes the focus, as it was asked for; from a work's cover,
    // the album's cover takes it, as after a click on it
    pub(super) fn go_to_album(&self, album_id: &str, track: Option<&gio::File>) {
        self.set_page(LibraryPage::Albums.name());
        let library_view = &self.imp().library_view;
        // The whole album opens, so the Tracks toggle goes off: left on,
        // the pane would list the whole album while a click on its cover
        // took only the tracks that match
        library_view.set_tracks_only(false);
        let Some(track) = track else {
            library_view.open_album(album_id);
            return;
        };
        if let Some(album) = library_view.reveal_album(album_id) {
            self.show_album_tracks(
                &album,
                TrackFilter::default(),
                PickNames::default(),
                Some(track),
            );
        }
    }

    // With an empty queue, there is nothing to play, so the album grid
    // takes the place of the player, and there's no going to it
    pub(super) fn update_library_for_queue(&self) {
        // The queue is only empty while the tracks of a playlist take the
        // place of those of the previous one, and showing the grid in the
        // meantime would make it flash
        if self.imp().pending_playlist.borrow().is_some() {
            return;
        }

        let queue_empty = self.queue_is_empty();
        if self.imp().player_available.replace(!queue_empty) == queue_empty {
            self.notify_player_available();
        }

        // At startup, the queue is empty until it's restored, or until
        // the files to open are added, and showing the grid in the meantime
        // would only make it flash; the playlists stay over the grid, e.g.
        // while switching from a deleted one
        if queue_empty && !self.imp().starting.get() {
            self.set_library_visible(true);
            self.set_playlist_visible(false);
        }
    }

    // Adds the tracks of the albums selected in the grid to the queue,
    // or plays them in its place; for the albums selected in a search, only
    // the tracks that match it
    fn queue_selected_albums(&self, play: bool) {
        let albums = self.imp().library_view.take_selected_albums();
        self.queue_albums(albums, play);
    }

    // Adds the tracks of albums to the queue, or plays them in its place
    fn queue_albums(&self, albums: Vec<(String, TrackFilter)>, play: bool) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Some(files) = win.album_files(albums).await else {
                    return;
                };

                if play {
                    win.play_shown_tracks(files, 0);
                } else {
                    win.queue_tracks(files, QueueMode::Add);
                }
            }
        ));
    }

    // Adds to the end of a section of a menu the item that goes to the work
    // of a track on the Works page, with the work open and the cover of the
    // track's recording pointed out among its covers. It stays off until
    // the returned `GoToWork` is given the work, which the menu looks up as
    // it opens, so it stays off for a track that isn't in the library.
    // `path` is the track's, whose recording it points out
    pub(super) fn add_go_to_work_item(
        &self,
        section: &gio::Menu,
        actions: &gio::SimpleActionGroup,
        prefix: &str,
        path: Option<PathBuf>,
    ) -> GoToWork {
        let go_to_work = LookedUpAction::new(
            actions,
            "go-to-work",
            clone!(
                #[weak(rename_to = win)]
                self,
                move |(key, album): &FoundWork| {
                    win.go_to_work(key, album.as_deref(), path.as_deref());
                }
            ),
        );
        section.append(
            Some(&i18n("Go to Work")),
            Some(&format!("{prefix}.go-to-work")),
        );
        go_to_work
    }

    // Goes to a work on the Works page, which opens it, gliding to the
    // cover of the recording of `track` on `album`, if any, which takes the
    // focus and flashes, or else to the work, which takes it; the collapsed
    // split view hides the pane, which covers the page
    pub(super) fn go_to_work(&self, key: &str, album: Option<&str>, track: Option<&Path>) {
        self.set_page(LibraryPage::Works.name());
        if !self.imp().library_view.shows_works() {
            return;
        }
        if self.imp().split_view.is_collapsed() {
            self.set_playlist_visible(false);
        }
        self.imp().library_view.reveal_work(key, album, track);
    }

    // The menu of a work, at a point of `widget` in its header in the queue
    // or among the tracks of an album, with its first movement: Go to Work
    // (see `add_go_to_work_item()`) and, after it, the item that marks the
    // work as a favorite. The work and the album are looked up before the
    // menu opens, in a separate thread, see `MenuLookups`
    pub(super) fn show_work_menu(
        &self,
        header: &gtk::ListHeader,
        widget: &gtk::Widget,
        x: f64,
        y: f64,
        track: &Track,
    ) {
        let actions = gio::SimpleActionGroup::new();
        let section = gio::Menu::new();
        let mut lookups = MenuLookups::new(&section);
        let path = track.file().path();
        let go_to_work = self.add_go_to_work_item(&section, &actions, "work", path.clone());
        if let Some(path) = path {
            let position = section.n_items();
            let hidden = self.imp().library_view.hidden_folders();
            let adder = lookups.adder();
            lookups.spawn(clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                section,
                #[weak]
                actions,
                async move {
                    let looked_up = gio::spawn_blocking(move || {
                        look_up_album_and_work(path, &hidden, false, true)
                    })
                    .await;
                    let Ok((album, Some(key))) = looked_up else {
                        return;
                    };
                    let item = win.favorite_work_item(&actions, "work", &key);
                    adder.insert_item(&section, position, &item);
                    go_to_work.set((key, album));
                }
            ));
        }
        // Unless the header shows another work by then, as the list
        // scrolled. A track whose tags were read again is the same object,
        // so it's still the same work
        lookups.open_when_done(
            self,
            clone!(
                #[weak]
                header,
                #[weak]
                widget,
                #[strong]
                track,
                move |win: &Window| {
                    let shown = header.item().and_downcast::<Track>();
                    if widget.is_mapped() && shown.as_ref() == Some(&track) {
                        popup_menu(win, &section, "work", &actions, &widget, x, y);
                    }
                }
            ),
        );
    }

    // The menu of a work of the list of works, at a point of its row: it
    // opens or closes to show the covers of its albums, as activating it
    // does, and it can be marked as a favorite
    fn show_works_row_menu(&self, work: &library::WorkInfo, widget: &gtk::Widget, x: f64, y: f64) {
        let library_view = self.imp().library_view.get();
        let actions = gio::SimpleActionGroup::new();
        let show = gio::SimpleAction::new("show", None);
        show.connect_activate(clone!(
            #[weak]
            library_view,
            #[strong]
            work,
            move |_, _| library_view.toggle_work(&work)
        ));
        actions.add_action(&show);
        let section = gio::Menu::new();
        let label = if library_view.is_work_open(&work.key) {
            i18n("Hide Recordings")
        } else {
            i18n("Show Recordings")
        };
        section.append(Some(&label), Some("works-row.show"));
        section.append_item(&self.favorite_work_item(&actions, "works-row", &work.key));
        popup_menu(self, &section, "works-row", &actions, widget, x, y);
    }

    // Plays tracks in place of the queue, from the one at `start`, as an
    // album's Play button does. The toast with an Undo only shows when the
    // queue had tracks that these leave out: a click on another track of
    // the album that's playing loses nothing, and the album's tracks are
    // mostly clicked for that. Once no tracks are read, see `after_loads()`
    pub(super) fn play_shown_tracks(&self, files: Vec<gio::File>, start: u32) {
        self.after_loads(move |win| {
            // With the queue selected, its tracks are the ones in the
            // player, and the queue as it was is only read for an Undo that
            // would show
            let keeps_queue = win.selected_playlist().is_empty()
                && !selected_playlist::loses_tracks(&win.player().queue().files(), &files);
            let replaced = (!keeps_queue)
                .then(|| win.replaced_queue())
                .filter(|replaced| replaced.loses_tracks(&files));
            win.replace_queue_with(files, start, replaced, None);
        });
    }

    // The menu of an album, at a point of its cell. On an album selected in
    // selection mode, its items take all the selected ones, and leave
    // selection mode like the buttons of its bar; on another, only that
    // one. With the Tracks toggle on, a search takes only the tracks that
    // match it, as elsewhere, and on the Works page a cover takes its
    // recording's
    fn show_album_menu(&self, cell: &AlbumCell, x: f64, y: f64) {
        let Some(album) = cell.album() else {
            return;
        };
        let library_view = self.imp().library_view.get();
        let selection = library_view.selection_mode() && album.selected();
        let album_id = album.id();
        let filter = library_view.taken_by(cell).0;
        let albums = move |win: &Self| {
            if selection {
                win.imp().library_view.take_selected_albums()
            } else {
                vec![(album_id.clone(), filter.clone())]
            }
        };
        let albums = Rc::new(albums);

        // Off while tracks are read, like the other ways to add them
        let enabled = self.imp().loads.get() == 0;
        let actions = gio::SimpleActionGroup::new();
        let play = gio::SimpleAction::new("play", None);
        play.set_enabled(enabled);
        play.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            albums,
            move |_, _| win.queue_albums(albums(&win), true)
        ));
        actions.add_action(&play);
        self.add_adding_actions(
            &actions,
            enabled,
            clone!(
                #[strong]
                albums,
                move |win| Some(Tracks::Albums(albums(win)))
            ),
        );

        let section = gio::Menu::new();
        section.append(Some(&i18n("Play")), Some("album.play"));
        self.append_adding_items(&section, "album", true, None);
        let menu = gio::Menu::new();
        menu.append_section(None, &section);
        let mut lookups = MenuLookups::new(&menu);
        // Among the covers of the recordings of a work, an album of its own
        // can be gone to on the Albums page, and the recording marked as a
        // favorite
        if let Some(work) = cell.work().filter(|_| !selection) {
            let go = gio::SimpleAction::new("go-to-album", None);
            let album_id = album.id();
            go.connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.go_to_album(&album_id, None)
            ));
            actions.add_action(&go);

            // As its heart shows
            let favorite = cell.favorite();
            let mark = gio::SimpleAction::new("favorite-recording", None);
            mark.connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.mark_favorite_recording(&work.recording, !favorite)
            ));
            actions.add_action(&mark);
            let label = if favorite {
                i18n("Remove Recording from Favorites")
            } else {
                i18n("Add Recording to Favorites")
            };
            let section = gio::Menu::new();
            section.append(Some(&i18n("Go to Album")), Some("album.go-to-album"));
            section.append(Some(&label), Some("album.favorite-recording"));
            menu.append_section(None, &section);
        }
        // An album of the grid, or the selected ones, can be marked as
        // favorites; they're unmarked when they all are. Not among the
        // covers of a work, where the heart is a favorite recording's
        if cell.work().is_none() {
            let ids: Vec<String> = if selection {
                library_view
                    .selected_albums()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            } else {
                vec![album.id()]
            };
            let favorite = ids.iter().all(|id| library_view.is_favorite_album(id));
            let label = match (ids.len() > 1, favorite) {
                (false, false) => i18n("Add Album to Favorites"),
                (false, true) => i18n("Remove Album from Favorites"),
                (true, false) => i18n("Add Albums to Favorites"),
                (true, true) => i18n("Remove Albums from Favorites"),
            };
            let mark = gio::SimpleAction::new("favorite-album", None);
            mark.connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                #[strong]
                albums,
                move |_, _| {
                    // Taken like the other items take them, which leaves
                    // selection mode
                    let ids: Vec<String> = albums(&win).into_iter().map(|(id, _)| id).collect();
                    win.mark_favorite_albums(&ids, !favorite);
                }
            ));
            actions.add_action(&mark);
            let section = gio::Menu::new();
            section.append(Some(&label), Some("album.favorite-album"));
            menu.append_section(None, &section);
        }
        // Its folder, and its booklets once they're found, for an album of
        // its own, as in the menu of a track
        if !selection {
            let open_directory = gio::SimpleAction::new("open-directory", None);
            let album_id = album.id();
            open_directory.connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        win,
                        #[strong]
                        album_id,
                        async move { win.open_album_directory(album_id).await }
                    ));
                }
            ));
            actions.add_action(&open_directory);

            let section = gio::Menu::new();
            section.append(Some(&i18n("Open Folder")), Some("album.open-directory"));
            menu.append_section(None, &section);
            let album_id = album.id();
            let hidden = library_view.hidden_folders();
            self.add_booklet_item(&section, &mut lookups, move || {
                library::album_tracks(&album_id, &TrackFilter::default(), &hidden)
            });
        }
        // At once, as its only lookup, the booklets', adds its item at the
        // end of the last section, which an open menu takes, see
        // `MenuLookups`; so the cell still shows the album and the work it
        // was asked for
        lookups.open_now(self, |win| {
            popup_menu(win, &menu, "album", &actions, cell.upcast_ref(), x, y);
        });
    }

    // Adds the tracks of albums dragged from the grid to the queue, before
    // a track, or at the end
    pub(super) fn add_dragged_albums(&self, drag: AlbumDrag, before: Option<Track>) {
        // Like the Add button, which the selected ones could have gone to
        if drag.selection {
            self.imp().library_view.set_selection_mode(false);
        }
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                if let Some(files) = win.album_files(drag.albums).await {
                    win.load_into_queue(
                        files,
                        QueueMode::Add,
                        LoadOptions {
                            place: before.map_or(Place::End, Place::Before),
                            ..Default::default()
                        },
                    );
                }
            }
        ));
    }

    pub(super) fn setup_library(&self) {
        let library_view = self.imp().library_view.get();

        // Albums are dropped on the playlist pane, which is shown while
        // they're dragged, and hidden again if they weren't dropped
        library_view.connect_albums_drag_begin(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                if !win.playlist_visible() {
                    win.show_playlist_view();
                    win.set_playlist_visible(true);
                    win.imp().pane_opened_for_drag.set(true);
                }
            }
        ));
        library_view.connect_albums_drag_end(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                if win.imp().pane_opened_for_drag.take() {
                    win.set_playlist_visible(false);
                }
            }
        ));

        // The menus of the Add buttons, which are only on while tracks
        // can be added
        let actions = gio::SimpleActionGroup::new();
        self.add_adding_actions(&actions, true, |win| {
            let albums = win.imp().library_view.take_selected_albums();
            (!albums.is_empty()).then_some(Tracks::Albums(albums))
        });
        library_view.insert_action_group("albums", Some(&actions));
        let actions = gio::SimpleActionGroup::new();
        self.add_adding_actions(&actions, true, |win| {
            let files = win.imp().album_tracks.files();
            (!files.is_empty()).then_some(Tracks::Shown(files))
        });
        let album_tracks = self.imp().album_tracks.get();
        album_tracks.insert_action_group("album-tracks", Some(&actions));

        library_view.connect_album_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, cell, x, y| win.show_album_menu(cell, x, y)
        ));

        library_view.connect_album_activated(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, album, filter, picks| {
                win.show_album_tracks(album, filter.clone(), picks.clone(), None);
            }
        ));

        album_tracks.connect_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, row, x, y| {
                win.show_track_menu(row, x, y, false);
            }
        ));
        album_tracks.connect_back(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.show_pane_back()
        ));
        album_tracks.connect_add(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks| {
                win.show_playlist_view();
                win.queue_tracks(album_tracks.files(), QueueMode::Add);
            }
        ));
        // The Play button plays the tracks it takes from their start, and a
        // track the same tracks from it, see `play_shown()`
        album_tracks.connect_play(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, pos, clicked| win.play_shown(pos, clicked)
        ));
        album_tracks.connect_favorite_toggled(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks, favorite| {
                if let Some(album_id) = album_tracks.album_id() {
                    win.mark_favorite_albums(&[album_id], favorite);
                }
            }
        ));
        album_tracks.connect_work_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, header, widget, x, y, track| win.show_work_menu(header, widget, x, y, track)
        ));
        library_view.connect_work_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, work, widget, x, y| win.show_works_row_menu(work, widget, x, y)
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
        library_view.connect_scan_finished(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, uris, uris_without_cover, n_changed, requested| {
                win.reload_changed_tracks(uris, uris_without_cover);
                if requested {
                    let msg = if n_changed == 0 {
                        i18n("No changes found in the music folders")
                    } else {
                        ni18n_k(
                            // Translators: the `{n}` must be left unmodified
                            "{n} track changed in the music folders",
                            "{n} tracks changed in the music folders",
                            n_changed,
                            &[("n", &n_changed.to_string())],
                        )
                    };
                    win.add_toast(msg);
                }
            }
        ));
        // The playing track's album and work can be in the library now, or
        // in a folder that's hidden, see `setup_more_button()`. Once the
        // albums are read again, not when the scan finishes, as the folders
        // hidden are those of the last reload, which the menu's lookups
        // take, and a scan of a drive just plugged in can change nothing
        library_view.connect_library_changed(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.refresh_more_menu()
        ));

        // The search entry takes Esc, so the window doesn't get it. With
        // text, Esc clears the search and leaves selection mode, as the
        // tracks selected in the queue would otherwise stay selected; in an
        // empty search, it shows the albums of all the composers and
        // performers again, or does what it does elsewhere in the window
        library_view.search_entry().connect_stop_search(clone!(
            #[weak(rename_to = win)]
            self,
            move |entry| {
                let library_view = win.imp().library_view.get();
                if entry.text().is_empty() && library_view.has_picks() {
                    win.exit_selection_mode();
                    library_view.clear_picks();
                } else if entry.text().is_empty() {
                    win.escape();
                } else {
                    win.exit_selection_mode();
                    entry.set_text("");
                }
            }
        ));

        // Reading the albums can take a while, so we do it in the
        // background as soon as the window is up
        library_view.start_scan(false);
    }

    // The files of the tracks of albums, each with what its tracks have to
    // match: the search and the composer picked, or nothing for the whole
    // album, without the tracks in the folders the grid hides; reading them
    // from the library takes a while, so it's done in a separate thread
    pub(super) async fn album_files(
        &self,
        albums: Vec<(String, TrackFilter)>,
    ) -> Option<Vec<gio::File>> {
        let hidden = self.imp().library_view.hidden_folders();
        let paths = gio::spawn_blocking(move || {
            albums
                .iter()
                .flat_map(|(id, filter)| library::album_tracks(id, filter, &hidden))
                .collect::<Vec<_>>()
        })
        .await
        .ok()?;
        Some(paths.into_iter().map(gio::File::for_path).collect())
    }

    // Opens the folder of an album, with its first track selected, or its
    // sheet, or the folder of its first disc for an album whose discs are
    // in folders of their own; for one whose tracks are in several other
    // folders, the folder of its first track
    async fn open_album_directory(&self, album_id: String) {
        let hidden = self.imp().library_view.hidden_folders();
        let entry = gio::spawn_blocking(move || {
            library::album_tracks(&album_id, &TrackFilter::default(), &hidden)
                .first()
                .map(|path| library::album_folder_entry(path).to_owned())
        })
        .await
        .ok()
        .flatten();
        // The album can be gone by now, if the music folders were read again
        let Some(entry) = entry else {
            self.add_toast(i18n("Unable to open folder"));
            return;
        };
        self.open_containing_folder(&gio::File::for_path(entry))
            .await;
    }
}

// How long a menu waits for what it looks up before it opens, see
// `MenuLookups`
const MENU_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

// What a menu looks up in separate threads as it's asked for: the album,
// work and booklets of a track or an album. A lookup that ends after the
// menu opened still turns its item on, but only adds one at the end of
// the menu: one added above other items would push them down, under the
// pointer as it reaches them. So the menus that have an item to add in
// the middle, as a track's Open Booklet is, or whose items the focus
// should start on, as a work's Go to Work, wait for their lookups before
// they open (`open_when_done()`), up to `MENU_WAIT` on a slow disk; one
// whose items found later all go at its end opens at once (`open_now()`),
// and the More menu is made before it's asked for (see
// `Window::setup_more_button()`)
pub(super) struct MenuLookups {
    adder: MenuAdder,
    tasks: Vec<glib::JoinHandle<()>>,
}

impl MenuLookups {
    pub(super) fn new(menu: &gio::Menu) -> Self {
        Self {
            adder: MenuAdder {
                menu: menu.clone(),
                open: Rc::default(),
                held_back: Rc::default(),
            },
            tasks: Vec::new(),
        }
    }

    pub(super) fn spawn(&mut self, lookup: impl Future<Output = ()> + 'static) {
        self.tasks.push(glib::spawn_future_local(lookup));
    }

    // What the lookups add items to the menu with
    pub(super) fn adder(&self) -> MenuAdder {
        self.adder.clone()
    }

    // Opens the menu with `open` once the lookups are done, or after
    // `MENU_WAIT`, unless another menu was asked for by then. The lookups
    // that are left go on, as dropping their handles leaves them running
    pub(super) fn open_when_done(self, win: &Window, open: impl FnOnce(&Window) + 'static) {
        let request = new_menu_request(win);
        let Self { adder, tasks } = self;
        glib::spawn_future_local(clone!(
            #[weak]
            win,
            async move {
                let done = async move {
                    for task in tasks {
                        let _ = task.await;
                    }
                };
                let _ = glib::future_with_timeout(MENU_WAIT, done).await;
                if win.imp().menu_requests.get() == request {
                    adder.set_open(true);
                    open(&win);
                }
            }
        ));
    }

    // Opens the menu with `open` at once, for one whose lookups only add
    // items at its end
    pub(super) fn open_now(self, win: &Window, open: impl FnOnce(&Window)) {
        self.adder.set_open(true);
        open(win);
    }
}

// Counts a menu asked for or opened, so that one still waiting for its
// lookups doesn't open after it, see `MenuLookups::open_when_done()`
pub(super) fn new_menu_request(win: &Window) -> u32 {
    let requests = &win.imp().menu_requests;
    let request = requests.get().wrapping_add(1);
    requests.set(request);
    request
}

// Adds the items that a menu's lookups find, see `MenuLookups`
#[derive(Clone)]
pub(super) struct MenuAdder {
    menu: gio::Menu,
    open: Rc<std::cell::Cell<bool>>,
    held_back: Rc<std::cell::Cell<bool>>,
}

impl MenuAdder {
    // Says whether the menu is open. A popover made for one opening stays
    // open until it goes, but the More menu opens and closes again
    pub(super) fn set_open(&self, open: bool) {
        self.open.set(open);
    }

    // Inserts `item` into a section of the menu, or into the menu itself,
    // unless the menu is open and the item wouldn't be its last
    pub(super) fn insert_item(&self, section: &gio::Menu, position: i32, item: &gio::MenuItem) {
        if !self.open.get() || self.ends_menu(section, position) {
            section.insert_item(position, item);
        } else {
            self.held_back.set(true);
        }
    }

    // Whether an item was left out as the menu was open, which the More
    // menu, made again once it closes, gets then
    pub(super) fn held_back(&self) -> bool {
        self.held_back.get()
    }

    fn ends_menu(&self, section: &gio::Menu, position: i32) -> bool {
        if position < section.n_items() {
            return false;
        }
        if *section == self.menu {
            return true;
        }
        let last = self.menu.n_items() - 1;
        last >= 0
            && self
                .menu
                .item_link(last, gio::MENU_LINK_SECTION)
                .is_some_and(|link| link == *section.upcast_ref::<gio::MenuModel>())
    }
}

// Opens a menu at a point of a widget, with its actions in a group. A
// menu still waiting for its lookups doesn't open after it
pub(super) fn popup_menu(
    win: &Window,
    menu: &gio::Menu,
    group: &str,
    actions: &gio::SimpleActionGroup,
    parent: &gtk::Widget,
    x: f64,
    y: f64,
) {
    new_menu_request(win);
    let point = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
    let popover = utils::menu_popover(menu, parent, Some(&point));
    popover.insert_action_group(group, Some(actions));
    popover.set_halign(gtk::Align::Start);
    popover.popup();
}

// The work's key and the album's id, which Go to Work goes to
pub(super) type FoundWork = (String, Option<String>);

// Looks up in the library a track's album, if `album_wanted`, and the key
// of its work, if `work_wanted`, with its album too, on which Go to Work
// points out the recording. It blocks, so it runs in a separate thread
pub(super) fn look_up_album_and_work(
    path: PathBuf,
    hidden: &[PathBuf],
    album_wanted: bool,
    work_wanted: bool,
) -> (Option<String>, Option<String>) {
    let work = work_wanted
        .then(|| library::track_work(&path, hidden))
        .flatten();
    let album = (album_wanted || work.is_some())
        .then(|| library::track_album(&path, hidden))
        .flatten();
    (album, work)
}

// The Go to Work item of a menu, which is off until its work is found
pub(super) type GoToWork = LookedUpAction<FoundWork>;

// The action of a menu's item that's off until what it acts on is looked
// up, see `MenuLookups`
pub(super) struct LookedUpAction<T> {
    action: glib::WeakRef<gio::SimpleAction>,
    found: Rc<std::cell::OnceCell<T>>,
}

impl<T: 'static> LookedUpAction<T> {
    // Adds the action to `actions`, off; once on, it calls `activate`
    // with what was found
    pub(super) fn new(
        actions: &gio::SimpleActionGroup,
        name: &str,
        activate: impl Fn(&T) + 'static,
    ) -> Self {
        let found: Rc<std::cell::OnceCell<T>> = Rc::default();
        let action = gio::SimpleAction::new(name, None);
        action.set_enabled(false);
        action.connect_activate(clone!(
            #[strong]
            found,
            move |_, _| {
                if let Some(found) = found.get() {
                    activate(found);
                }
            }
        ));
        actions.add_action(&action);
        Self {
            action: action.downgrade(),
            found,
        }
    }

    // Turns the item on, unless its menu is gone by then
    pub(super) fn set(&self, found: T) {
        if let Some(action) = self.action.upgrade() {
            let _ = self.found.set(found);
            action.set_enabled(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(menu: &gio::Menu) -> Vec<String> {
        (0..menu.n_items())
            .filter_map(|i| {
                menu.item_attribute_value(i, gio::MENU_ATTRIBUTE_LABEL, None)
                    .and_then(|label| label.get::<String>())
            })
            .collect()
    }

    fn item(label: &str) -> gio::MenuItem {
        gio::MenuItem::new(Some(label), None)
    }

    #[test]
    fn an_open_menu_only_takes_items_at_its_end() {
        let first = gio::Menu::new();
        first.append(Some("Copy"), None);
        first.append(Some("Track Information"), None);
        let last = gio::Menu::new();
        last.append(Some("Remove"), None);
        let menu = gio::Menu::new();
        menu.append_section(None, &first);
        menu.append_section(None, &last);
        let adder = MenuLookups::new(&menu).adder();

        // Before it opens, anywhere
        adder.insert_item(&first, 1, &item("Open Booklet"));
        assert!(!adder.held_back());
        assert_eq!(
            labels(&first),
            ["Copy", "Open Booklet", "Track Information"]
        );

        // Once it's open, not above other items, even at the end of a
        // section that another one follows
        adder.set_open(true);
        adder.insert_item(&first, 1, &item("Above"));
        adder.insert_item(&first, 3, &item("Before the next section"));
        assert_eq!(
            labels(&first),
            ["Copy", "Open Booklet", "Track Information"]
        );
        adder.insert_item(&last, 0, &item("Above Remove"));
        assert_eq!(labels(&last), ["Remove"]);
        assert!(adder.held_back());

        // At the end of the last section, or of a menu without sections
        adder.insert_item(&last, 1, &item("Last"));
        assert_eq!(labels(&last), ["Remove", "Last"]);

        // Closed again, as the More menu is, anywhere again
        adder.set_open(false);
        adder.insert_item(&first, 0, &item("Go to Album"));
        assert_eq!(
            labels(&first),
            ["Go to Album", "Copy", "Open Booklet", "Track Information"]
        );
        let plain = gio::Menu::new();
        plain.append(Some("Go to Work"), None);
        let adder = MenuLookups::new(&plain).adder();
        adder.set_open(true);
        adder.insert_item(&plain, 1, &item("Add Work to Favorites"));
        assert_eq!(labels(&plain), ["Go to Work", "Add Work to Favorites"]);
    }
}
