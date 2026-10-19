// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

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

    pub(super) fn setup_library(&self) {
        let library_view = self.imp().library_view.get();

        library_view.connect_album_activated(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, album| {
                win.show_album_tracks(album, &win.imp().library_view.search_words(), None);
            }
        ));

        let album_tracks = self.imp().album_tracks.get();
        album_tracks.connect_menu_requested(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, row, x, y| {
                win.show_track_menu(row, x, y);
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

        // The search entry takes Esc to clear the search, so the window
        // does not get it to leave selection mode; do both, as the tracks
        // selected in the queue would otherwise stay selected
        library_view.search_entry().connect_stop_search(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                win.exit_selection_mode();
            }
        ));

        // Reading the albums can take a while, so we do it in the
        // background as soon as the window is up
        library_view.start_scan();
    }
}
