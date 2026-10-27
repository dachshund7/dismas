// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{adding::Tracks, *};

impl Window {
    // Shows the tracks of an album in the sidebar, in place of the
    // queue, so that it can be played or added to the queue
    fn show_album_tracks(
        &self,
        album: &Album,
        search_words: &[String],
        reveal: Option<&gio::File>,
    ) {
        let imp = self.imp();
        if imp.sidebar_stack.visible_child_name().as_deref() != Some("album") {
            imp.sidebar_was_visible.set(self.playlist_visible());
        }
        imp.album_tracks.show_album(album, search_words, reveal);
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
        self.show_album_tracks(&album, &[], Some(track));
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
    fn queue_albums(&self, albums: Vec<(String, Vec<String>)>, play: bool) {
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
                vec![(album_id.clone(), library_view.track_words())]
            }
        };
        let albums = Rc::new(albums);

        let actions = gio::SimpleActionGroup::new();
        let play = gio::SimpleAction::new("play", None);
        play.set_enabled(self.imp().loads.get() == 0);
        play.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            albums,
            move |_, _| win.queue_albums(albums(&win), true)
        ));
        actions.add_action(&play);
        let enabled = self.imp().loads.get() == 0;
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

        let popover = gtk::PopoverMenu::from_model(Some(&section));
        popover.insert_action_group("album", Some(&actions));
        popover.set_parent(cell);
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
                win.show_album_tracks(album, &win.imp().library_view.search_words(), None);
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
        album_tracks.connect_play(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks, start| {
                win.select_queue_to_play();
                win.clear_queue();
                win.show_playlist_view();
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
        // empty search, it does what it does elsewhere in the window
        library_view.search_entry().connect_stop_search(clone!(
            #[weak(rename_to = win)]
            self,
            move |entry| {
                if entry.text().is_empty() {
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

// The files of the tracks of albums, each with the words its tracks have
// to match: those of the search, or none for the whole album; reading
// them from the library takes a while, so it's done in a separate thread
pub(super) async fn album_files(albums: Vec<(String, Vec<String>)>) -> Option<Vec<gio::File>> {
    let paths = gio::spawn_blocking(move || {
        albums
            .iter()
            .flat_map(|(id, words)| library::album_tracks(id, words))
            .collect::<Vec<_>>()
    })
    .await
    .ok()?;
    Some(paths.into_iter().map(gio::File::for_path).collect())
}
