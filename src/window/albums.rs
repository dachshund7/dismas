// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{adding::Tracks, *};

impl Window {
    // Shows the tracks of an album in the sidebar, in place of the
    // queue, so that it can be played or added to the queue; with a
    // filter, only those it matches, see `AlbumTracks::show_album()`
    fn show_album_tracks(
        &self,
        album: &Album,
        filter: &TrackFilter,
        picks: &PickNames,
        reveal: Option<&gio::File>,
    ) {
        let imp = self.imp();
        if imp.sidebar_stack.visible_child_name().as_deref() != Some("album") {
            imp.sidebar_was_visible.set(self.playlist_visible());
        }
        imp.album_tracks.show_album(album, filter, picks, reveal);
        imp.sidebar_stack.set_visible_child_name("album");
        self.set_playlist_visible(true);
    }

    // Shows the recordings of a work in the sidebar, those the filter
    // matches, like the tracks of an album
    fn show_work_recordings(
        &self,
        work: &library::WorkInfo,
        filter: &TrackFilter,
        reveal: Option<&gio::File>,
    ) {
        let imp = self.imp();
        if imp.sidebar_stack.visible_child_name().as_deref() != Some("album") {
            imp.sidebar_was_visible.set(self.playlist_visible());
        }
        let hidden = imp.library_view.hidden_folders();
        imp.album_tracks.show_work(work, filter, hidden, reveal);
        imp.sidebar_stack.set_visible_child_name("album");
        self.set_playlist_visible(true);
    }

    // Shows all the tracks of an album in the sidebar, scrolled to one of
    // them, which flashes; the page stays, and the grid only scrolls to the
    // album when it's showing
    pub(super) fn go_to_album(&self, album_id: &str, track: &gio::File) {
        let library_view = &self.imp().library_view;
        let album = if self.library_visible() {
            library_view.reveal_album(album_id)
        } else {
            library_view.album(album_id)
        };
        let Some(album) = album else {
            return;
        };
        self.show_album_tracks(
            &album,
            &TrackFilter::default(),
            &PickNames::default(),
            Some(track),
        );
    }

    // Goes back from the tracks of an album to the queue, closing the
    // sidebar if it was opened for the album
    fn show_playlist(&self) {
        self.show_playlist_view();
        if !self.imp().sidebar_was_visible.get() {
            self.set_playlist_visible(false);
        }
    }

    pub(super) fn show_playlist_view(&self) {
        self.imp().sidebar_stack.set_visible_child_name("playlist");
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
    // the tracks that match it, unless the whole albums are taken
    fn queue_selected_albums(&self, play: bool) {
        let library_view = self.imp().library_view.get();
        let albums = library_view.selected_albums();
        library_view.set_selection_mode(false);
        self.queue_albums(albums, play);
    }

    // Adds the tracks of albums to the queue, or plays them in its place
    fn queue_albums(&self, albums: Vec<(String, TrackFilter)>, play: bool) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Some(files) = album_files(albums).await else {
                    return;
                };

                if play {
                    win.select_queue_to_play();
                    win.clear_queue();
                    win.show_playlist_view();
                    win.queue_tracks(files, QueueMode::Play(0));
                } else {
                    win.queue_tracks(files, QueueMode::Add);
                }
            }
        ));
    }

    // An item of a menu that shows the recordings of the work of a track in
    // the sidebar, with the number of the others; the work is looked up
    // while the menu opens, and the item stays off for a track that isn't
    // in the library, or whose work has no other recording. Its own
    // recording is among them, and the track flashes
    pub(super) fn add_other_recordings_item(
        &self,
        section: &gio::Menu,
        actions: &gio::SimpleActionGroup,
        prefix: &str,
        track: &Track,
    ) {
        let work: Rc<RefCell<Option<library::WorkInfo>>> = Rc::default();
        let action = gio::SimpleAction::new("other-recordings", None);
        action.set_enabled(false);
        action.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            work,
            #[strong]
            track,
            move |_, _| {
                if let Some(work) = work.borrow().as_ref() {
                    win.show_work_recordings(work, &TrackFilter::default(), Some(&track.file()));
                }
            }
        ));
        actions.add_action(&action);
        let position = section.n_items();
        let detailed = format!("{prefix}.other-recordings");
        section.append(Some(&i18n("Other Recordings")), Some(&detailed));

        let Some(path) = track.file().path() else {
            return;
        };
        let hidden = self.imp().library_view.hidden_folders();
        glib::spawn_future_local(clone!(
            #[weak]
            action,
            #[weak]
            section,
            async move {
                let found = gio::spawn_blocking(move || library::track_work(&path, &hidden))
                    .await
                    .ok()
                    .flatten();
                let Some((info, others)) = found.filter(|(_, others)| *others > 0) else {
                    return;
                };
                work.replace(Some(info));
                section.remove(position);
                let label = i18n_k(
                    // Translators: `{n}` must be left unmodified; it is
                    // expanded to the number of the other recordings of
                    // the work of a track in the music folders
                    "Other Recordings ({n})",
                    &[("n", &others.to_string())],
                );
                section.insert(position, Some(&label), Some(&detailed));
                action.set_enabled(true);
            }
        ));
    }

    // The menu of a work, at a point of its header in the queue or among
    // the tracks of an album, with its first movement
    pub(super) fn show_work_menu(&self, widget: &gtk::Widget, x: f64, y: f64, track: &Track) {
        let actions = gio::SimpleActionGroup::new();
        let section = gio::Menu::new();
        self.add_other_recordings_item(&section, &actions, "work", track);
        popup_menu(&section, "work", &actions, widget, x, y);
    }

    // Plays tracks of the sidebar in place of the queue, from the one at
    // `start`, as an album's Play button does
    fn play_shown_tracks(&self, files: Vec<gio::File>, start: u32) {
        self.select_queue_to_play();
        self.clear_queue();
        self.show_playlist_view();
        self.queue_tracks(files, QueueMode::Play(start));
    }

    // The menu of a recording of a work in the sidebar, at a point of its
    // header: its tracks are played, added, or shown on their album
    fn show_recording_menu(&self, widget: &gtk::Widget, x: f64, y: f64, start: u32, end: u32) {
        let album_tracks = self.imp().album_tracks.get();
        let files = album_tracks.section_files(start, end);
        let Some(first) = files.first().cloned() else {
            return;
        };
        let album_id = album_tracks.recording_album(start);

        let enabled = self.imp().loads.get() == 0;
        let actions = gio::SimpleActionGroup::new();
        let play = gio::SimpleAction::new("play", None);
        play.set_enabled(enabled);
        play.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            files,
            move |_, _| win.play_shown_tracks(files.clone(), 0)
        ));
        actions.add_action(&play);
        self.add_adding_actions(&actions, enabled, move |_| {
            Some(Tracks::ShownAlbum(files.clone()))
        });
        let go_to_album = gio::SimpleAction::new("go-to-album", None);
        go_to_album.set_enabled(album_id.is_some());
        go_to_album.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(album_id) = &album_id {
                    win.go_to_album(album_id, &first);
                }
            }
        ));
        actions.add_action(&go_to_album);

        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some(&i18n("Play")), Some("recording.play"));
        section.append(Some(&i18n("Play Next")), Some("recording.play-next"));
        let item = gio::MenuItem::new(Some(&self.add_to_selected_label()), None);
        item.set_action_and_target_value(
            Some("recording.add-to"),
            Some(&self.selected_playlist().to_variant()),
        );
        section.append_item(&item);
        section.append_submenu(
            Some(&i18n("Add to Playlist")),
            &self.add_to_menu("recording"),
        );
        menu.append_section(None, &section);
        let section = gio::Menu::new();
        section.append(Some(&i18n("Go to Album")), Some("recording.go-to-album"));
        menu.append_section(None, &section);
        popup_menu(&menu, "recording", &actions, widget, x, y);
    }

    // The menu of an album, at a point of its cell. On an album selected in
    // selection mode, its items take all the selected ones, and leave
    // selection mode like the buttons of its bar; on another, only that
    // one. A search takes only the tracks that match it, as elsewhere
    fn show_album_menu(&self, album: &Album, cell: &gtk::Widget, x: f64, y: f64) {
        let library_view = self.imp().library_view.get();
        let selection = library_view.selection_mode() && album.selected();
        let album_id = album.id();
        let albums = move |win: &Self| {
            let library_view = win.imp().library_view.get();
            if selection {
                let albums = library_view.selected_albums();
                library_view.set_selection_mode(false);
                albums
            } else {
                vec![(album_id.clone(), library_view.track_filter())]
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
        self.add_adding_actions(&actions, enabled, move |win| {
            Some(Tracks::Albums(albums(win)))
        });

        let section = gio::Menu::new();
        section.append(Some(&i18n("Play")), Some("album.play"));
        section.append(Some(&i18n("Play Next")), Some("album.play-next"));
        let item = gio::MenuItem::new(Some(&self.add_to_selected_label()), None);
        item.set_action_and_target_value(
            Some("album.add-to"),
            Some(&self.selected_playlist().to_variant()),
        );
        section.append_item(&item);
        section.append_submenu(Some(&i18n("Add to Playlist")), &self.add_to_menu("album"));
        popup_menu(&section, "album", &actions, cell, x, y);
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
                if let Some(files) = album_files(drag.albums).await {
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
            let library_view = win.imp().library_view.get();
            let albums = library_view.selected_albums();
            library_view.set_selection_mode(false);
            (!albums.is_empty()).then_some(Tracks::Albums(albums))
        });
        library_view.insert_action_group("albums", Some(&actions));
        let actions = gio::SimpleActionGroup::new();
        self.add_adding_actions(&actions, true, |win| {
            let files = win.imp().album_tracks.files();
            (!files.is_empty()).then_some(Tracks::ShownAlbum(files))
        });
        self.imp()
            .album_tracks
            .insert_action_group("album-tracks", Some(&actions));

        library_view.connect_album_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, album, cell, x, y| win.show_album_menu(album, cell, x, y)
        ));

        library_view.connect_album_activated(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, album| {
                let library_view = win.imp().library_view.get();
                win.show_album_tracks(
                    album,
                    &library_view.search_filter(),
                    &library_view.search_pick_names(),
                    None,
                );
            }
        ));

        // Whole Albums is one setting, which the album's tracks follow, and
        // which their banner switches too
        let album_tracks = self.imp().album_tracks.get();
        library_view
            .bind_property("whole-albums", &album_tracks, "whole-albums")
            .bidirectional()
            .sync_create()
            .build();
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
            move |_| win.show_playlist()
        ));
        album_tracks.connect_add(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks| {
                win.show_playlist_view();
                win.queue_tracks(album_tracks.files(), QueueMode::Add);
            }
        ));
        // A track of a work's recordings plays that recording
        album_tracks.connect_play(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks, pos| {
                let (files, start) = album_tracks.files_to_play(pos);
                win.play_shown_tracks(files, start);
            }
        ));
        album_tracks.connect_work_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, widget, x, y, track| win.show_work_menu(widget, x, y, track)
        ));
        album_tracks.connect_recording_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, widget, x, y, start, end| win.show_recording_menu(widget, x, y, start, end)
        ));
        library_view.connect_work_activated(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, work, filter| win.show_work_recordings(work, filter, None)
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
            move |_, uris, n_changed, requested| {
                win.reload_changed_tracks(&uris);
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
}

// The files of the tracks of albums, each with what its tracks have to
// match: the search and the composer picked, or nothing for the whole
// album; reading them from the library takes a while, so it's done in a
// separate thread
pub(super) async fn album_files(albums: Vec<(String, TrackFilter)>) -> Option<Vec<gio::File>> {
    let paths = gio::spawn_blocking(move || {
        albums
            .iter()
            .flat_map(|(id, filter)| library::album_tracks(id, filter))
            .collect::<Vec<_>>()
    })
    .await
    .ok()?;
    Some(paths.into_iter().map(gio::File::for_path).collect())
}

// Opens a menu at a point of a widget, with its actions in a group
fn popup_menu(
    menu: &gio::Menu,
    group: &str,
    actions: &gio::SimpleActionGroup,
    parent: &gtk::Widget,
    x: f64,
    y: f64,
) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.insert_action_group(group, Some(actions));
    popover.set_parent(parent);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    // The menu item is activated after the popover closes, so it can only
    // go once the action ran
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}
