// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The tracks of a playlist, shown in the pane on the page of an album's
// tracks, to look at them without switching to it or playing them. Only
// one list shows in the pane at a time, so a playlist that isn't selected
// is shown there over the queue, and the selected one is the queue itself,
// where its tracks are loaded and can be changed. Play on a saved playlist
// switches to it, which leaves the queue as it was, in its own file; on a
// smart playlist, it plays its tracks in place of the queue, with an Undo

use super::*;

impl Window {
    // Asks for other tracks on the tracks page, so that a playlist still
    // being read for it no longer is wanted. Going anywhere else in the pane
    // does this too, or the playlist would cover what was asked for once
    // it's read
    pub(super) fn request_shown(&self) -> u32 {
        let imp = self.imp();
        imp.shown_reading.take();
        let request = imp.shown_request.get().wrapping_add(1);
        imp.shown_request.set(request);
        request
    }

    // The playlist the tracks page shows from now on, as the tracks show,
    // or none for an album's or a smart playlist's tracks
    pub(super) fn set_shown_playlist(&self, playlist: Option<String>) {
        let imp = self.imp();
        if imp.shown_playlist.replace(playlist.clone()) != playlist {
            // The Add button's menu leaves out the playlist shown
            imp.album_tracks.set_add_menu(
                self.add_button_menu("album-tracks", playlist.as_deref())
                    .upcast_ref(),
            );
        }
    }

    // The name of the playlist the tracks page shows, if it shows one
    pub(super) fn shown_playlist(&self) -> Option<String> {
        self.imp().shown_playlist.borrow().clone()
    }

    fn shown_is_current(&self, request: u32) -> bool {
        self.imp().shown_request.get() == request
    }

    // Whether this playlist is being read for the tracks page
    fn reading_preview_of(&self, name: &str) -> bool {
        self.imp().shown_reading.borrow().as_deref() == Some(name)
    }

    // Shows the tracks of a playlist in the pane, without switching to it.
    // The selected one shows as the queue, where it's loaded, and so does
    // the one being switched to, whose tracks are being read for the queue.
    // Its file is read in a separate thread, as it can be on a slow drive
    pub(super) fn show_playlist_preview(&self, name: &str) {
        let request = self.request_shown();
        let pending = self.imp().pending_playlist.borrow().clone();
        if name == self.selected_playlist() || pending.as_deref() == Some(name) {
            self.show_playlist_view();
            self.set_playlist_visible(true);
            return;
        }
        if !name.is_empty() && !playlists::exists(name) {
            self.add_toast(i18n("Unable to open the playlist"));
            return;
        }

        let name = name.to_string();
        self.imp().shown_reading.replace(Some(name.clone()));
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let read = {
                    let name = name.clone();
                    gio::spawn_blocking(move || playlists::load(&name)).await
                };
                if !win.shown_is_current(request) {
                    return;
                }
                win.imp().shown_reading.take();
                let Ok(Some(files)) = read else {
                    win.add_toast(i18n("Unable to open the playlist"));
                    return;
                };
                let title = selected_playlist::playlist_title(&name).into_owned();
                win.set_shown_playlist(Some(name));
                win.imp().album_tracks.show_files(&title, "", files, true);
                win.open_pane_page(pane::PanePage::Tracks);
            }
        ));
    }

    // Shows the tracks a smart playlist picks now, without playing them
    pub(super) fn show_smart_playlist_preview(&self, kind: SmartPlaylist) {
        let request = self.request_shown();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let files = win.smart_playlist_files(kind).await;
                if !win.shown_is_current(request) {
                    return;
                }
                let Some(files) = files else {
                    return;
                };
                win.set_shown_playlist(None);
                win.imp()
                    .album_tracks
                    .show_files(&kind.title(), &kind.description(), files, false);
                win.open_pane_page(pane::PanePage::Tracks);
            }
        ));
    }

    // The Play button of the tracks page, or a click on its track at `pos`:
    // an album and a smart playlist play the tracks shown in place of the
    // queue; a saved playlist is switched to and plays, from where it was,
    // or from the track clicked. The Playlists page can be over the player
    // in either case
    pub(super) fn play_shown(&self, pos: u32, clicked: bool) {
        let album_tracks = self.imp().album_tracks.get();
        self.set_playlists_visible(false);
        match self.shown_playlist() {
            None => {
                let files = album_tracks.files();
                // The track clicked is among the ones that could be read,
                // so its place among the files can differ
                let start = clicked
                    .then(|| album_tracks.track(pos))
                    .flatten()
                    .and_then(|track| files.iter().position(|f| f.equal(&track.file())))
                    .unwrap_or(0);
                self.play_shown_tracks(files, start as u32);
            }
            Some(name) => {
                // A track clicked is where it goes on from. Its place is its
                // place in the playlist, as the tracks shown keep those of
                // the files that aren't there; in selection mode, the Play
                // button takes the first selected track
                let start = if clicked {
                    Some(pos)
                } else if album_tracks.selection_mode() {
                    album_tracks
                        .tracks()
                        .iter()
                        .position(Track::selected)
                        .map(|index| index as u32)
                } else {
                    None
                };
                let start = start.and_then(|index| {
                    let track = album_tracks.track(index)?;
                    Some(ResumePoint {
                        uri: track.uri(),
                        index,
                        position: 0,
                    })
                });
                self.play_playlist(&name, start);
            }
        }
    }

    // Tracks were added at the end of a playlist's file: if it's the one
    // shown, they're added there too, for the Play and Add buttons as well.
    // A read going on could have missed them, so it starts over
    pub(super) fn shown_playlist_added_to(&self, name: &str, files: &[gio::File]) {
        if self.reading_preview_of(name) {
            self.show_playlist_preview(name);
        } else if self.shown_playlist().as_deref() == Some(name) {
            self.imp().album_tracks.add_files(files);
        }
    }

    // A playlist was renamed: if it's the one shown, its tracks stay, under
    // the new name. A read of it going on starts over under that name
    pub(super) fn shown_playlist_renamed(&self, name: &str, new_name: &str) {
        if self.reading_preview_of(name) {
            self.show_playlist_preview(new_name);
        }
        if self.shown_playlist().as_deref() != Some(name) {
            return;
        }
        self.set_shown_playlist(Some(new_name.to_string()));
        self.imp().album_tracks.set_title(new_name);
    }

    // A playlist was deleted: if it's the one shown, the pane goes back from
    // its tracks, or leaves them out of the pages under the one that shows.
    // A read of it going on is no longer wanted
    pub(super) fn shown_playlist_deleted(&self, name: &str) {
        if self.reading_preview_of(name) {
            self.request_shown();
        }
        if self.shown_playlist().as_deref() != Some(name) {
            return;
        }
        self.set_shown_playlist(None);
        if self.shown_pane_page() == Some(pane::PanePage::Tracks) {
            self.show_pane_back();
        } else {
            self.drop_pane_page(pane::PanePage::Tracks);
        }
    }
}
