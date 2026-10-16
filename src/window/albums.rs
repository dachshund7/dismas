// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // Shows the tracks of an album in the sidebar, in place of the
    // queue, so that it can be played or added to the queue
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
    pub(super) fn go_to_album(&self, album_id: &str) {
        // Showing the grid focuses the search, so it goes first, for the
        // album to take the focus
        self.set_library_visible(true);
        let Some(album) = self.imp().library_view.reveal_album(album_id) else {
            return;
        };
        self.show_album_tracks(&album, &[]);
    }

    // Goes back from the tracks of an album to the queue, closing the
    // sidebar if it was opened for the album
    fn show_playlist(&self) {
        self.show_playlist_view();
        if !self.imp().sidebar_was_visible.get() {
            self.set_playlist_visible(false);
        }
    }

    fn show_playlist_view(&self) {
        self.imp().sidebar_stack.set_visible_child_name("playlist");
    }

    // With an empty queue, there is nothing to play, so the album grid
    // takes the place of the player, and there's no going back to it
    pub(super) fn update_library_for_queue(&self) {
        let queue_empty = self.queue_is_empty();
        self.imp().library_view.set_can_go_back(!queue_empty);

        // At startup, the queue is empty until it's restored, or until
        // the files to open are added, and showing the grid in the meantime
        // would only make it flash
        if queue_empty && !self.imp().starting.get() {
            self.set_library_visible(true);
            self.set_playlist_visible(false);
        }
    }

    // Adds the tracks of the albums selected in the grid to the queue,
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
                    win.show_playlist_view();
                    win.queue_tracks(files, QueueMode::Play(0));
                } else {
                    win.queue_tracks(files, QueueMode::Add);
                }
            }
        ));
    }

    pub(super) fn setup_library(&self) {
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
                win.show_playlist_view();
                win.queue_tracks(album_tracks.files(), QueueMode::Add);
            }
        ));
        album_tracks.connect_play(clone!(
            #[weak(rename_to = win)]
            self,
            move |album_tracks, start| {
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

        // Reading the albums can take a while, so we do it in the
        // background as soon as the window is up
        library_view.start_scan();
    }
}
