// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::selected_playlist::start_in_music_folder;
use super::*;
use crate::{cue_sheet, library, playlists};

// The types of the files that are tracks, or give some: CUE sheets split
// audio files into tracks, and WebM files are often audio alone
const TRACK_TYPES: [&str; 3] = ["audio/*", "application/x-cue", "video/webm"];

impl Window {
    // Called once the queue of the last session, or the files the
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

        let pending = imp.after_startup.take();
        for f in pending {
            f(self);
        }
    }

    // The playlist of the last session is read at startup, at the same
    // time as tracks loaded meanwhile, which would land in the middle of it
    // and be stored without it, so they wait until it's in the queue.
    // While it's read, they wait with what waits for the load, such as a
    // track clicked meanwhile, so that they all run in the order they were
    // asked for, once startup has ended; the end of a load runs without
    // yielding, so what waits for startup alone is older than all of those
    fn after_startup(&self, f: impl FnOnce(&Self) + 'static) {
        let imp = self.imp();
        if !imp.starting.get() {
            f(self);
        } else if imp.loads.get() > 0 {
            self.after_loads(f);
        } else {
            imp.after_startup.borrow_mut().push(Box::new(f));
        }
    }

    // Runs `f` once no tracks are being read for the queue, or looked for
    // in folders (`find_audio_files()`), right away if none are, so that
    // loads happen one at a time. A switch to another
    // playlist, tracks played in place of the queue, or tracks added, while
    // other tracks are read, would otherwise go among them, or take the
    // place of tracks that aren't in the queue yet: tracks played while a
    // playlist was read for a switch were saved in that playlist's file
    pub(super) fn after_loads(&self, f: impl FnOnce(&Self) + 'static) {
        let imp = self.imp();
        if imp.loads.get() > 0 {
            debug!("Waiting for the tracks being read for the queue");
            imp.after_loads.borrow_mut().push_back(Box::new(f));
        } else {
            f(self);
        }
    }

    // Once a load is done, runs what waited for it, in the order asked,
    // until one of them reads tracks in turn, which the rest wait for
    fn run_after_loads(&self) {
        while self.imp().loads.get() == 0 {
            let Some(f) = self.imp().after_loads.borrow_mut().pop_front() else {
                return;
            };
            f(self);
        }
    }

    // Opens the playlist selected in the last session: a saved one always,
    // and the queue if the user wants it; otherwise, it starts empty
    pub(crate) fn restore_playlist_on_startup(&self) {
        // A window opened again while playing in the background
        if !self.queue_is_empty() {
            self.end_startup();
            return;
        }

        if !self.imp().settings.boolean("restore-playlist") {
            // The queue of the last session is gone, rather than coming
            // back when switching to the queue
            playlists::save("", &[]);
            playlists::drop_unshuffled("");
            self.sync_queue_shuffled();
            if self.selected_playlist().is_empty() {
                self.end_startup();
                return;
            }
        }
        self.restore_playlist();
    }

    // Goes on from where playback was, if the track is still there
    fn restore_playlist(&self) {
        let name = self.selected_playlist();
        match playlists::load(&name) {
            // The saved queue is empty after clearing the queue
            Some(files) if !files.is_empty() => {
                let resume = ResumePoint::load(&self.imp().settings, &name);
                self.load_into_queue(
                    files,
                    QueueMode::Restore,
                    LoadOptions {
                        resume,
                        ..Default::default()
                    },
                );
            }
            // Unreadable: adding tracks would replace the ones in its file.
            // The queue is always there, so this doesn't loop
            None if !name.is_empty() => {
                self.set_selected_playlist("");
                self.restore_playlist();
            }
            _ => self.end_startup(),
        }
    }

    // Adds the files to the end of the queue
    pub(super) fn queue_tracks(&self, queue: Vec<gio::File>, mode: QueueMode) {
        self.load_into_queue(queue, mode, LoadOptions::default());
    }

    // Puts the tracks of a playlist in the queue, in place of those of the
    // selected one, once they're read; it goes on from `resume`, paused, or
    // playing with `play`
    pub(super) fn queue_playlist_tracks(
        &self,
        queue: Vec<gio::File>,
        name: &str,
        play: bool,
        resume: Option<ResumePoint>,
    ) {
        self.load_into_queue(
            queue,
            QueueMode::Restore,
            LoadOptions {
                switch_to: Some(name.to_string()),
                resume,
                play,
                ..Default::default()
            },
        );
    }

    // The actions on the queue and on playlists are off while tracks are
    // read; what's asked for meanwhile in other ways waits, see
    // `after_loads()`
    fn set_loading(&self, loading: bool) {
        let imp = self.imp();
        let loads = imp.loads.get();
        let loads = if loading {
            loads + 1
        } else {
            loads.saturating_sub(1)
        };
        imp.loads.set(loads);
        // The first load turns them off, and the last one on again
        let first_or_last = if loading { loads == 1 } else { loads == 0 };
        if !first_or_last {
            return;
        }

        self.update_queue_actions();
        for action in [
            "win.select-playlist",
            "win.play-random",
            "win.new-playlist",
            "win.rename-playlist",
            "win.delete-playlist",
            "win.save-queue",
        ] {
            self.action_set_enabled(action, !loading);
        }
        // The menu of the pane's title switches with an action of its own,
        // which isn't on the class
        if let Some(action) = self
            .lookup_action("selected-playlist")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(!loading);
        }
        imp.album_tracks.set_adding_enabled(!loading);
        imp.library_view.set_adding_enabled(!loading);
        if loading {
            imp.playlist_view.begin_loading();
        } else {
            imp.playlist_view.end_loading();
        }
    }

    // Reads the tracks and adds them to the queue, as `mode` and `options`
    // say, once no other tracks are being read
    pub(super) fn load_into_queue(
        &self,
        queue: Vec<gio::File>,
        mode: QueueMode,
        options: LoadOptions,
    ) {
        self.after_loads(move |win| win.start_load(queue, mode, options));
    }

    fn start_load(&self, queue: Vec<gio::File>, mode: QueueMode, options: LoadOptions) {
        let LoadOptions {
            switch_to,
            resume,
            play: play_from_resume,
            replaced,
            place,
            announce,
        } = options;
        let play = matches!(mode, QueueMode::Play(_));
        // The file to start playing from; its place among the tracks read
        // can differ, as a file before it can fail to be read
        let start_file = match mode {
            QueueMode::Play(start) => queue.get(start as usize).cloned(),
            _ => None,
        };
        let replacing = replaced.is_some();
        if queue.is_empty() {
            self.add_toast(i18n("No available track found"));
            self.end_startup();
            return;
        }

        self.set_loading(true);
        // At startup, the pane is as the user last showed or hid it, with the
        // queue of the last session or with the files Dismas was opened
        // with. Opening it as the window first shows would also make the
        // window wider by the pane, which it would keep
        let at_startup =
            self.imp().starting.get() && matches!(mode, QueueMode::Restore | QueueMode::Show);
        if mode != QueueMode::Quiet && !at_startup {
            self.set_playlist_visible(true);
        }

        // Begin the trace
        let now = Instant::now();

        // The tracks are read in a separate thread, and added all at once
        let n_files = queue.len() as u32;
        // The tracks of a playlist that aren't there keep their places
        let receiver = load_tracks(queue, mode == QueueMode::Restore);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let mut tracks = Vec::new();
                let mut cur_file: u32 = 0;
                while let Ok(data) = receiver.recv().await {
                    cur_file += 1;
                    win.imp().playlist_view.update_loading(cur_file, n_files);
                    tracks.extend(data.into_iter().map(Track::new));
                }

                debug!(
                    "Total loading time for {} files: {} ms",
                    n_files,
                    now.elapsed().as_millis()
                );

                win.set_loading(false);

                // The tracks of the playlist being selected replace those
                // of the previous one, even when none of them was found;
                // the queue is only empty in between when some were, so
                // it stays pending until they're added
                if let Some(name) = &switch_to {
                    if tracks.is_empty() {
                        win.imp().pending_playlist.take();
                    }
                    win.replace_selected_playlist(name);
                }

                // Replacing tells about it even without tracks, as the
                // queue was emptied already
                if let Some(replaced) = replaced {
                    win.add_replaced_toast(tracks.len(), replaced, announce);
                } else if tracks.is_empty() {
                    win.add_toast(i18n("No tracks found"));
                }

                if !tracks.is_empty() {
                    let player = win.player();
                    let queue = player.queue();
                    let was_empty = queue.is_empty();
                    let first_added = queue.n_tracks();

                    // Tracks added while a search hides some would rarely
                    // match it, and go unseen, so it's closed and the list
                    // shows where they went; not for tracks added from the
                    // command line, which leave the pane as it is
                    let close_search = !matches!(mode, QueueMode::Quiet | QueueMode::Restore)
                        && win.queue_filtering();
                    if close_search {
                        win.close_queue_search();
                    }

                    // Bulk add to avoid hammering the UI with list model updates
                    let before = match &place {
                        Place::End => None,
                        Place::Before(track) => Some(track.clone()),
                        Place::Next => queue.play_next_before(&[]),
                    };
                    queue.insert_tracks(&tracks, before.as_ref());
                    // The list keeps the rows it shows in place, so tracks put
                    // above the first of them would be out of its view; the
                    // list goes to the track that plays instead
                    let placed = !matches!(place, Place::End);
                    if placed || (close_search && !play) {
                        win.reveal_queue_track(&tracks[0]);
                    }

                    // Store the current state of the queue; restoring
                    // doesn't change it
                    if mode != QueueMode::Restore {
                        win.store_queue();
                    }

                    debug!(
                        "Queue was empty: {}, new size: {}",
                        was_empty,
                        queue.n_tracks()
                    );
                    if play {
                        let start = start_file
                            .and_then(|file| tracks.iter().position(|t| t.file().equal(&file)))
                            .unwrap_or(0);
                        player.skip_to(first_added + start as u32);
                        player.play();
                    } else if was_empty {
                        let resume_at = resume.and_then(|r| {
                            let uris: Vec<String> = tracks.iter().map(Track::uri).collect();
                            Some((r.find_track(&uris)?, r.position))
                        });
                        // A track whose file isn't there can't play, so it
                        // goes on from the start of the next one that can
                        let (pos, position) = resume_at.unwrap_or_default();
                        if let Some(first) = queue.playable_from(pos) {
                            player.skip_to(first);
                            if first == pos && resume_at.is_some() {
                                player.resume_at(position.into());
                            }
                            if play_from_resume {
                                player.play();
                            }
                        }
                    }
                    // The tracks played next after these go after them,
                    // also into an empty queue, where they start it
                    let play_next = matches!(place, Place::Next);
                    if play_next {
                        queue.mark_played_next(&tracks);
                    }
                    match mode {
                        // Show where the album went, even if the
                        // queue was closed while adding it
                        QueueMode::Add => win.set_playlist_visible(true),
                        QueueMode::Quiet => {}
                        _ => win.set_library_visible(false),
                    }

                    // Restoring the queue at startup is quiet, and
                    // doesn't start playing; replacing has a toast of its own
                    if mode == QueueMode::Restore {
                        // Nothing to announce
                    } else if replacing {
                        // Its toast is up already
                    } else if tracks.len() == 1 {
                        // Allow jumping to the track we just added
                        // If we added a single track, and the queue was empty, we
                        // dispense with the pleasantries and we start playing
                        // immediately; otherwise, we let the user choose whether
                        // to jump to the newly added track
                        if was_empty || play {
                            player.play();
                        } else {
                            let msg = if play_next {
                                i18n("The track will play next")
                            } else {
                                i18n("Added a new track")
                            };
                            win.add_skip_to_toast(msg, i18n("Play"), &tracks[0]);
                        }
                    } else if play_next && !was_empty {
                        let msg = ni18n_f(
                            // Translators: the `{}` must be left unmodified;
                            // it will be expanded to the number of tracks put
                            // in the queue to play after the playing one
                            "One track will play next",
                            "{} tracks will play next",
                            tracks.len() as u32,
                            &[&tracks.len().to_string()],
                        );
                        win.add_toast(msg);
                    } else {
                        let msg = ni18n_f(
                            // Translators: the `{}` must be left unmodified;
                            // it will be expanded to the number of tracks added
                            // to the queue
                            "Added one track",
                            "Added {} tracks",
                            tracks.len() as u32,
                            &[&tracks.len().to_string()],
                        );

                        win.add_toast(msg);
                    }
                }

                if switch_to.is_some() && win.imp().pending_playlist.take().is_some() {
                    win.update_library_for_queue();
                }

                win.end_startup();
                win.run_after_loads();
            }
        ));
    }

    // Looks for the audio files among `files` and in the folders, which
    // takes a while, in a separate thread, and passes them to `then`. The
    // search counts as a load, so that what's asked for meanwhile waits for
    // it and for the tracks `then` reads, in the order asked: otherwise,
    // as the search returns at once, a track clicked meanwhile, or a
    // playlist switched to, would go first, and the files would then take
    // its place, or go into the other playlist. The search itself waits
    // for the tracks being read, which come first
    fn find_audio_files(
        &self,
        files: Vec<gio::File>,
        then: impl FnOnce(&Self, Vec<gio::File>) + 'static,
    ) {
        self.after_loads(move |win| {
            win.set_loading(true);
            glib::spawn_future_local(clone!(
                #[weak]
                win,
                async move {
                    let music_folders = library::music_folder_paths();
                    let found = gio::spawn_blocking(move || audio_files(files, &music_folders))
                        .await
                        .unwrap_or_default();
                    // `then` starts its load right away, as no other is on
                    win.set_loading(false);
                    then(&win, found);
                    win.run_after_loads();
                }
            ));
        });
    }

    // Adds the audio files, and the ones in the folders, to the queue, at
    // the end or before a track
    fn add_files_to_queue(&self, files: Vec<gio::File>, mode: QueueMode, before: Option<Track>) {
        self.find_audio_files(files, move |win, queue| {
            win.load_into_queue(
                queue,
                mode,
                LoadOptions {
                    place: before.map_or(Place::End, Place::Before),
                    ..Default::default()
                },
            );
        });
    }

    // Plays the audio files, and the ones in the folders, in place of the
    // tracks of the queue, like an album's Play button, for files dropped
    // or from the command line; nothing is replaced until some are found
    fn play_dropped_files(&self, files: Vec<gio::File>) {
        self.find_audio_files(files, |win, queue| {
            if queue.is_empty() {
                win.add_toast(i18n("No available track found"));
                return;
            }
            win.play_in_place_of_queue(queue);
        });
    }

    // Plays the tracks in place of those of the queue, selecting it if a
    // saved playlist was; the toast's Undo puts them back
    pub(super) fn play_in_place_of_queue(&self, files: Vec<gio::File>) {
        self.play_announced_in_place_of_queue(files, None);
    }

    // The same, with a toast that says `announce`, if any, rather than how
    // many tracks there are. Once no tracks are read, as the queue they take
    // the place of, which the Undo puts back, would be missing those
    pub(super) fn play_announced_in_place_of_queue(
        &self,
        files: Vec<gio::File>,
        announce: Option<String>,
    ) {
        self.after_loads(move |win| {
            let replaced = win.replaced_queue();
            win.replace_queue_with(files, 0, Some(replaced), announce);
        });
    }

    // Empties the queue for the tracks, selecting it if a saved playlist
    // was, and plays them from the one at `start`. With `replaced`, the
    // toast that follows has an Undo that puts it back. It's called once no
    // tracks are read, see `after_loads()`
    pub(super) fn replace_queue_with(
        &self,
        files: Vec<gio::File>,
        start: u32,
        replaced: Option<ReplacedQueue>,
        announce: Option<String>,
    ) {
        self.select_queue_to_play();
        self.clear_queue();
        self.show_playlist_view();
        self.load_into_queue(
            files,
            QueueMode::Play(start),
            LoadOptions {
                replaced,
                announce,
                ..Default::default()
            },
        );
    }

    fn add_replaced_toast(
        &self,
        n_tracks: usize,
        replaced: ReplacedQueue,
        announce: Option<String>,
    ) {
        let msg = if n_tracks == 0 {
            i18n("No tracks found")
        } else if let Some(announce) = announce {
            announce
        } else {
            ni18n_f(
                // Translators: the `{}` must be left unmodified; it will be
                // expanded to the number of tracks that took the place of
                // those of the queue
                "Replaced the queue with one track",
                "Replaced the queue with {} tracks",
                n_tracks as u32,
                &[&n_tracks.to_string()],
            )
        };
        self.add_undo_toast(msg, move |win| win.undo_replace(&replaced));
    }

    // Reads the tags of the tracks in the queue and in the album shown in
    // the sidebar again when their files changed, e.g. when a scan of the
    // library finds new tags; the tracks are updated in place, so the
    // lists keep their order and the current track keeps playing. The
    // tracks whose files weren't found are looked for again, as a scan
    // follows plugging in a drive. The tracks of `uris_without_cover`, of
    // albums new to the library, are read again if they have no cover, as
    // they can have been read before their cover file came. It waits for
    // the tracks being read for the queue, which aren't in it yet: they
    // would be left out, and a later scan doesn't list them again, as when
    // a download is added while its cover is written
    pub(super) fn reload_changed_tracks(&self, uris: Vec<String>, uris_without_cover: Vec<String>) {
        self.after_loads(move |win| win.reload_tracks(&uris, &uris_without_cover));
    }

    fn reload_tracks(&self, uris: &[String], uris_without_cover: &[String]) {
        // Each track shown is looked up, and there can be thousands of
        // them, as of the tracks of a scan
        let uris: HashSet<&str> = uris.iter().map(String::as_str).collect();
        let without_cover: HashSet<&str> = uris_without_cover.iter().map(String::as_str).collect();
        let player = self.player();
        // The tracks shown, with whether their file changed
        let shown: Vec<(Track, bool)> = player
            .queue()
            .model()
            .iter::<Track>()
            .flatten()
            .chain(self.imp().album_tracks.tracks())
            .map(|track| {
                let uri = track.uri();
                let changed = uris.contains(uri.as_str())
                    || (without_cover.contains(uri.as_str()) && track.cover_uuid().is_none());
                (track, changed)
            })
            .collect();

        // The cover in their tags might have changed too, and the other
        // tracks of the album share it, so they are read again with them.
        // The tracks without a cover share the lack of one by folder, which
        // can be a whole folder of single tracks, so they aren't: when the
        // cover files of an album change, the scan lists all its tracks.
        // The cache keeps the lack of a cover too, which the tracks of the
        // album still hold, so it's forgotten as well, so that the tracks
        // read again look for their cover again
        let mut covers = HashSet::new();
        let mut forgotten = HashSet::new();
        for (track, _) in shown.iter().filter(|(_, changed)| *changed) {
            let Some(uuid) = track.album_cover_uuid() else {
                continue;
            };
            if track.cover_uuid().is_some() {
                covers.insert(uuid.clone());
            }
            forgotten.insert(uuid);
        }
        let tracks: Vec<Track> = shown
            .into_iter()
            .filter(|(track, changed)| {
                track.missing()
                    || *changed
                    || track
                        .cover_uuid()
                        .is_some_and(|uuid| covers.contains(&uuid))
            })
            .map(|(track, _)| track)
            .collect();
        if tracks.is_empty() {
            return;
        }
        {
            let mut cache = CoverCache::global().lock().unwrap();
            for uuid in &forgotten {
                cache.forget(uuid);
            }
        }

        let receiver = load_tracks(tracks.iter().map(Track::file).collect(), true);
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                for track in tracks {
                    let Ok(data) = receiver.recv().await else {
                        break;
                    };
                    // A file that can't be read anymore keeps its old tags,
                    // as does one that a CUE sheet splits now
                    let Ok([data]) = <[_; 1]>::try_from(data) else {
                        continue;
                    };
                    // Still not there, or still unreadable
                    if data.is_missing() && data.missing_reason() == track.missing_reason() {
                        continue;
                    }
                    track.update(data);
                    win.show_updated_track(&track);
                }
            }
        ));
    }

    // Shows the tags of a track that were read again, in the lists and, if
    // it's the playing track, in the player
    fn show_updated_track(&self, track: &Track) {
        // The cover can have changed without its id, which is the album's
        self.imp().full_cover.take();
        let player = self.player();
        player.queue().track_changed(track);
        self.imp().album_tracks.track_changed(track);
        self.imp().lyrics_view.track_changed(track);
        if player.state().track().as_ref() == Some(track) {
            player.current_track_changed();
            self.update_title(Some(track));
            self.update_style(Some(track));
            // Whether it's a movement can have changed, which the More
            // menu has an item for (see `setup_more_button()`); what
            // else it has is looked up in the library, whose reload
            // after the scan makes the menu again already
            if self.imp().more_menu_with_work.get() != track.has_work() {
                self.refresh_more_menu();
            }
        }
    }

    // Says that a track couldn't be played; the player went on to the next
    // track if it could. A track whose file is gone is greyed out from then
    // on, as it would be after the playlist opens again. Looking for the
    // file is done in another thread, as a network drive that's gone can
    // keep it waiting. Several tracks can fail one after the other, so the
    // toast takes the place of the one about the track before
    pub(super) fn report_failed_track(&self, track: &Track) {
        let title = track.title();
        let path = track.file().path();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            async move {
                let gone = gio::spawn_blocking(move || {
                    path.is_some_and(|path| !cue_sheet::location_exists(&path))
                })
                .await
                .unwrap_or(false);

                let message = if gone {
                    if !track.missing() {
                        track.mark_missing();
                        win.show_updated_track(&track);
                    }
                    i18n_k(
                        // Translators: `{track}` must be left unmodified; it
                        // is expanded to the title of a track
                        "“{track}” wasn't found",
                        &[("track", &title)],
                    )
                } else {
                    i18n_k(
                        // Translators: `{track}` must be left unmodified; it
                        // is expanded to the title of a track
                        "Unable to play “{track}”",
                        &[("track", &title)],
                    )
                };
                let imp = win.imp();
                if let Some(toast) = imp.failed_toast.upgrade() {
                    toast.dismiss();
                }
                let toast = utils::plain_toast(&message);
                imp.failed_toast.set(Some(&toast));
                imp.toast_overlay.add_toast(toast);
            }
        ));
    }

    pub(crate) fn open_files(&self, files: &[gio::File]) {
        // Files opened while the playlist of the last session is still read
        // wait for it, and are then added to it, as in a running Dismas: if
        // they took the place of the queue now, they'd go among its tracks
        let imp = self.imp();
        if imp.starting.get() && imp.loads.get() > 0 {
            let files = files.to_vec();
            self.after_startup(move |win| win.open_files(&files));
            return;
        }

        if files.is_empty() {
            self.add_toast(i18n("Unable to access files"));
            self.end_startup();
            return;
        }

        // At startup, the selected playlist isn't loaded, and the files
        // take the place of the queue instead, which isn't shuffled anymore
        if imp.starting.get() {
            self.select_queue_to_play();
            playlists::drop_unshuffled("");
            self.sync_queue_shuffled();
        }

        self.add_files_to_queue(files.to_vec(), QueueMode::Show, None);
    }

    // Add Tracks… and Add Folder…, in the main menu, add tracks from outside
    // the music folders to the selected playlist, like opening them from a
    // file manager, as a phone has nothing to drag them from
    pub(super) async fn add_tracks_from_dialog(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("Audio files")));
        for mime_type in TRACK_TYPES {
            filter.add_mime_type(mime_type);
        }
        filter.add_suffix("cue");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title(i18n("Open File"))
            .accept_label(i18n("_Add"))
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();
        start_in_music_folder(&dialog).await;
        let files = dialog.open_multiple_future(Some(self)).await;
        self.add_files_from_dialog(files);
    }

    pub(super) async fn add_folder_from_dialog(&self) {
        let dialog = gtk::FileDialog::builder()
            .title(i18n("Open Folder"))
            .accept_label(i18n("_Add Folder"))
            .modal(true)
            .build();
        start_in_music_folder(&dialog).await;
        let folders = dialog.select_multiple_folders_future(Some(self)).await;
        self.add_files_from_dialog(folders);
    }

    // The files or the folders picked in a dialog
    fn add_files_from_dialog(&self, picked: Result<gio::ListModel, glib::Error>) {
        let Ok(picked) = picked else {
            // Cancelled
            return;
        };
        let files: Vec<gio::File> = picked.iter::<gio::File>().flatten().collect();
        if files.is_empty() {
            self.add_toast(i18n("Unable to access files"));
            return;
        }
        self.add_files_to_queue(files, QueueMode::Show, None);
    }

    // Adds files from the command line to the end of the selected playlist,
    // leaving the page and the pane as they are
    pub(crate) fn add_files(&self, files: Vec<gio::File>) {
        self.after_startup(move |win| win.add_files_to_queue(files, QueueMode::Quiet, None));
    }

    // Plays files from the command line in place of the queue
    pub(crate) fn play_files(&self, files: Vec<gio::File>) {
        self.after_startup(move |win| win.play_dropped_files(files));
    }

    // A single target takes the files dropped anywhere on the window, and
    // the albums dragged from the grid, and decides from where they are
    // what they do: on X11 the widgets aren't told that the drag left them
    // once it's dropped, so a second target that took the drag without
    // getting the drop would keep it, and turn the next one down
    pub(super) fn setup_drop_target(&self) {
        let drop_target = gtk::DropTarget::builder()
            .name("file-drop-target")
            .actions(gdk::DragAction::COPY)
            .formats(
                &gdk::ContentFormatsBuilder::new()
                    .add_type(gdk::FileList::static_type())
                    .add_type(AlbumDrag::static_type())
                    .build(),
            )
            .build();

        let show = clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target: &gtk::DropTarget, x: f64, y: f64| {
                let drop = target.current_drop();
                let place = if utils::is_drag_cancelled(drop.as_ref()) {
                    None
                } else {
                    win.file_drop_at(x, y, drags_albums(drop.as_ref()))
                };
                let droppable = place.is_some();
                win.show_file_drop(place, x, y);
                if droppable {
                    gdk::DragAction::COPY
                } else {
                    gdk::DragAction::empty()
                }
            }
        );
        drop_target.connect_enter(show.clone());
        drop_target.connect_motion(show);
        // Once the files are dropped, or the drag leaves
        drop_target.connect_current_drop_notify(clone!(
            #[weak(rename_to = win)]
            self,
            move |target| {
                if target.current_drop().is_none() {
                    win.end_file_drop();
                }
            }
        ));

        drop_target.connect_drop(clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            false,
            move |target, value, x, y| {
                if utils::is_drag_cancelled(target.current_drop().as_ref()) {
                    return false;
                }

                // Albums only go to the pane, which keeps the pane open
                if let Ok(drag) = value.get::<AlbumDrag>() {
                    let Some(place) = win.file_drop_at(x, y, true) else {
                        return false;
                    };
                    win.imp().pane_opened_for_drag.set(false);
                    win.add_dragged_albums(drag, win.drop_before(&place));
                    return true;
                }

                let Ok(file_list) = value.get::<gdk::FileList>() else {
                    return false;
                };
                if file_list.files().is_empty() {
                    win.add_toast(i18n("Unable to access dropped files"));
                    return false;
                }

                let files = file_list.files();
                match win.file_drop_at(x, y, false) {
                    Some(FileDrop::Zones) if win.imp().drag_overlay.in_play_zone(x, y) => {
                        win.play_dropped_files(files);
                    }
                    Some(FileDrop::Zones) => {
                        win.add_files_to_queue(files, QueueMode::Show, None);
                    }
                    // Browsing the albums goes on
                    Some(place) => {
                        let before = win.drop_before(&place);
                        win.add_files_to_queue(files, QueueMode::Add, before);
                    }
                    None => return false,
                }
                true
            }
        ));

        self.imp().drag_overlay.add_controller(drop_target);
    }

    // On X11, the app dragging the files finds the window under the
    // pointer from a cache of the windows' places and shapes, which GTK
    // keeps for the app's life. A resize updates the place, but the shape
    // only changes with an event of its own, which a window without a
    // shape doesn't get: after the window was made wider, drags over the
    // new part went to the window below. The shape is set to the window
    // after each resize, so that the event comes. With a compositor, GTK
    // sets the shape itself, around the shadows, and it's left alone
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    pub(super) fn setup_x11_drop_shape(&self) {
        self.connect_realize(|win| {
            let Some(surface) = win.surface() else {
                return;
            };
            if !surface.is::<gdk_x11::X11Surface>() {
                return;
            }
            surface.connect_layout(|surface, width, height| {
                if surface.display().is_composited() {
                    return;
                }
                let rect = gtk::cairo::RectangleInt::new(0, 0, width, height);
                surface.set_input_region(Some(&gtk::cairo::Region::create_rectangle(&rect)));
            });
        });
    }

    // What files, or albums, dropped at a point of the window would do, if
    // they can be dropped there; the point is relative to the drag overlay,
    // which has the whole window. Files dropped elsewhere than on the pane
    // go to the zones; albums only go to the pane, since the grid has
    // buttons to add and play them
    fn file_drop_at(&self, x: f64, y: f64, albums: bool) -> Option<FileDrop> {
        let pane_drop = self.pane_drop_at(x, y);
        if albums {
            pane_drop
        } else {
            Some(pane_drop.unwrap_or(FileDrop::Zones))
        }
    }

    // Where in the queue something dropped at a point of the playlist pane
    // would go, if the point is on it
    fn pane_drop_at(&self, x: f64, y: f64) -> Option<FileDrop> {
        let imp = self.imp();
        let picked = imp.drag_overlay.pick(x, y, gtk::PickFlags::DEFAULT)?;
        let pane = imp.playlist_pane.upcast_ref::<gtk::Widget>();
        if picked != *pane && !picked.is_ancestor(pane) {
            return None;
        }
        Some(self.queue_drop_place(&picked, y).unwrap_or(FileDrop::End))
    }

    // The track that what's dropped goes before, or `None` for the end
    fn drop_before(&self, place: &FileDrop) -> Option<Track> {
        match place {
            FileDrop::Place(row, true) | FileDrop::AboveWork(row) => row.track(),
            FileDrop::Place(row, false) => {
                let target = row.track()?;
                self.player().queue().track_after(&target)
            }
            FileDrop::End | FileDrop::Zones => None,
        }
    }

    // Where in the queue files dropped at a point of the pane would go, if
    // not at the end. In the list, they go by the row under the point, or
    // the closest one, as between the rows and below the last one, the
    // point is on the list itself
    fn queue_drop_place(&self, picked: &gtk::Widget, y: f64) -> Option<FileDrop> {
        let imp = self.imp();
        let overlay = &*imp.drag_overlay;
        let queue_view = imp.playlist_view.queue_view();
        let list = queue_view.upcast_ref::<gtk::Widget>();
        if picked != list && !picked.is_ancestor(list) {
            return None;
        }

        let y = y as f32;
        let (row, bounds) = self
            .mapped_queue_rows()
            .into_iter()
            .filter(|bin| bin.parent().is_some_and(|item| item.is_child_visible()))
            .filter_map(|bin| bin.child().and_downcast::<QueueRow>())
            .filter_map(|row| {
                let bounds = row.compute_bounds(overlay)?;
                Some((row, bounds))
            })
            .min_by(|(_, a), (_, b)| distance(a, y).total_cmp(&distance(b, y)))?;
        let local_y = (y - bounds.y()).clamp(0.0, bounds.height());
        if let Some(above) = row.drop_position(f64::from(local_y)) {
            return Some(FileDrop::Place(row, above));
        }

        // Above the first movement of a work, which would be under its
        // header, the files go before the work: below the row before it,
        // as when moving tracks, or above the header of the first work
        if !row.reorderable() {
            return None;
        }
        let previous = row.position().and_then(|pos| {
            let track = queue_view.model()?.item(pos.checked_sub(1)?)?;
            self.mapped_queue_rows()
                .into_iter()
                .filter(|bin| bin.parent().is_some_and(|item| item.is_child_visible()))
                .filter_map(|bin| bin.child().and_downcast::<QueueRow>())
                .find(|row| {
                    row.track()
                        .is_some_and(|t| t.upcast_ref::<glib::Object>() == &track)
                })
        });
        Some(match previous {
            Some(previous) => FileDrop::Place(previous, false),
            None => FileDrop::AboveWork(row),
        })
    }

    // Shows what dropping at a point would do: a line between the tracks
    // of the queue where they'd go, the outline of the pane when they'd go
    // at the end, the zones, or nothing when they can't be dropped there
    fn show_file_drop(&self, place: Option<FileDrop>, x: f64, y: f64) {
        let imp = self.imp();
        let Some(drop) = place else {
            self.end_file_drop();
            return;
        };

        let row = match &drop {
            FileDrop::Place(row, _) => Some(row),
            _ => None,
        };
        if let Some(old) = imp.file_drop_row.upgrade()
            && Some(&old) != row
        {
            old.hide_drop_position();
        }
        imp.file_drop_row.set(row);
        if let FileDrop::Place(row, above) = &drop {
            row.show_drop_line(*above);
        }

        let header = match &drop {
            FileDrop::AboveWork(row) => row.work_header(),
            _ => None,
        };
        self.show_header_drop_line(header.as_ref());

        if matches!(drop, FileDrop::End) {
            imp.playlist_pane.add_css_class("drop-at-end");
        } else {
            imp.playlist_pane.remove_css_class("drop-at-end");
        }

        if matches!(drop, FileDrop::Zones) {
            let (left, right, blurred) = self.zones_area();
            imp.drag_overlay.show_zones(left, right, &blurred, x, y);
        } else {
            imp.drag_overlay.hide_zones();
        }
    }

    // Where the zones go across the window, and what is blurred under them:
    // the main area next to the playlist pane, where it shows, as tracks
    // dropped on the pane go in between its tracks. Next to the sidebar of the collapsed split view, the
    // main area is only drawn moved aside, so the zones are laid out where
    // it shows rather than over it
    fn zones_area(&self) -> (f32, f32, gtk::Widget) {
        let imp = self.imp();
        let overlay = &*imp.drag_overlay;
        let width = overlay.width() as f32;
        let whole = (0.0, width, imp.toast_overlay.get().upcast());
        if !imp.playlist_pane.is_mapped() {
            return whole;
        }
        let Some(pane) = imp.playlist_pane.compute_bounds(overlay) else {
            return whole;
        };
        let main = imp.content_stack.get().upcast();
        // The pane starts at the left edge, also as it slides in, or it's
        // on the right in right-to-left languages
        if pane.x() < 1.0 {
            (pane.x() + pane.width(), width, main)
        } else {
            (0.0, pane.x(), main)
        }
    }

    // A line above the header of a work, the one at the top of the queue,
    // where files would go; like the one of moving tracks, it's on the
    // content of the header
    fn show_header_drop_line(&self, header: Option<&gtk::Widget>) {
        let imp = self.imp();
        if let Some(old) = imp.file_drop_header.upgrade()
            && Some(&old) != header
            && let Some(content) = old.first_child()
        {
            content.remove_css_class("drop-above");
        }
        imp.file_drop_header.set(header);
        if let Some(content) = header.and_then(WidgetExt::first_child) {
            content.add_css_class("drop-above");
        }
    }

    // The queue isn't told when the drag leaves it after a drop, so it
    // stops scrolling here too
    pub(super) fn end_file_drop(&self) {
        let imp = self.imp();
        if let Some(row) = imp.file_drop_row.upgrade() {
            row.hide_drop_position();
        }
        imp.file_drop_row.set(None);
        self.show_header_drop_line(None);
        imp.playlist_pane.remove_css_class("drop-at-end");
        imp.drag_overlay.hide_zones();
        imp.playlist_view.stop_autoscroll();
    }
}

// Whether albums from the grid are dragged, rather than files
fn drags_albums(drop: Option<&gdk::Drop>) -> bool {
    drop.is_some_and(|drop| drop.formats().contains_type(AlbumDrag::static_type()))
}

// How far a height is from a row
fn distance(bounds: &graphene::Rect, y: f32) -> f32 {
    if y < bounds.y() {
        bounds.y() - y
    } else {
        (y - (bounds.y() + bounds.height())).max(0.0)
    }
}

// What files dropped at a point of the window do
enum FileDrop {
    // Added to the queue above or below the track of a row of the pane
    Place(QueueRow, bool),
    // Added before the work that a row starts, the one at the top of the
    // queue, as above its first movement would be under its header
    AboveWork(QueueRow),
    // Added at the end of the queue, dropped elsewhere on the pane
    End,
    // Added or played by the zones over the main area
    Zones,
}

// The audio files, the ones in the folders and their subfolders, and the
// tracks of the M3U playlists, as a file manager opens a playlist with
// Dismas, which are looked for in the music folders too, as for an
// imported one. The files and folders given, and the ones inside the
// folders, can be symbolic links, see `utils::read_folder_tree()`
fn audio_files(files: Vec<gio::File>, music_folders: &[std::path::PathBuf]) -> Vec<gio::File> {
    let mut queue = Vec::new();

    for file in files {
        // Tracks are read from their path, and asking GVFS about an http://
        // URI, given on the command line, would fetch it
        if file.path().is_none() {
            debug!("Skipping '{}', which has no local path", file.uri());
            continue;
        }
        if let Ok(info) = file.query_info(
            "standard::name,standard::display-name,standard::type,standard::content-type",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        ) {
            match info.file_type() {
                gio::FileType::Regular => {
                    let Some(content_type) = info.content_type() else {
                        continue;
                    };
                    if gio::content_type_is_mime_type(&content_type, "audio/x-mpegurl") {
                        debug!("Adding the tracks of the playlist '{}'", file.uri());
                        if let Some(tracks) = file
                            .path()
                            .and_then(|path| playlists::import_m3u(&path, music_folders))
                        {
                            queue.extend(tracks);
                        }
                    } else if TRACK_TYPES
                        .iter()
                        .any(|mime_type| gio::content_type_is_mime_type(&content_type, mime_type))
                    {
                        debug!("Adding file '{}' to the queue", file.uri());
                        queue.push(file);
                    }
                }
                // Without the hidden folders and recycle bins in it, as
                // the root of a drive has
                gio::FileType::Directory => {
                    debug!("Adding folder '{}' to the queue", file.uri());
                    queue.extend(utils::load_files_from_folder_filtered(
                        &file,
                        true,
                        &library::is_library_entry,
                    ));
                }
                _ => (),
            }
        }
    }

    queue
}
