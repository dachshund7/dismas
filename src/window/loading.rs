// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

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
    }

    // Opens the playlist selected in the last session: a saved one always,
    // and the queue if the user wants it; otherwise, it starts empty
    pub fn restore_playlist_on_startup(&self) {
        // A window opened again while playing in the background
        if !self.queue_is_empty() {
            self.end_startup();
            return;
        }

        if !self.imp().settings.boolean("restore-playlist") {
            // The queue of the last session is gone, rather than coming
            // back when switching to the queue
            playlists::save("", &[]);
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
                self.load_into_queue(files, QueueMode::Restore, None, resume, None);
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
        self.load_into_queue(queue, mode, None, None, None);
    }

    // Puts the tracks of a playlist in the queue, in place of those of the
    // selected one, once they're read; it goes on from where it was
    pub(super) fn queue_playlist_tracks(&self, queue: Vec<gio::File>, name: &str) {
        let resume = ResumePoint::load(&self.imp().settings, name);
        self.load_into_queue(
            queue,
            QueueMode::Restore,
            Some(name.to_string()),
            resume,
            None,
        );
    }

    // The actions on the queue are off while tracks are read, which can
    // happen more than once at a time, e.g. dropping files while the tracks
    // of a playlist are read; loading is "atomic"
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
            "win.new-playlist",
            "win.rename-playlist",
            "win.delete-playlist",
        ] {
            self.action_set_enabled(action, !loading);
        }
        imp.album_tracks.set_adding_enabled(!loading);
        imp.library_view.set_adding_enabled(!loading);
        if loading {
            imp.playlist_view.begin_loading();
        } else {
            imp.playlist_view.end_loading();
        }
    }

    // Reads the tracks and adds them to the queue; with `switch_to`, they
    // replace the tracks of the selected playlist with those of the one
    // named. Only this load does that, as files can be dropped meanwhile,
    // and their tracks go to the playlist that is selected when they're read.
    // With `resume`, playback is paused where it was, in the empty queue.
    // With `replaced`, the tracks took the place of those, and the toast
    // can put them back
    fn load_into_queue(
        &self,
        queue: Vec<gio::File>,
        mode: QueueMode,
        switch_to: Option<String>,
        resume: Option<ResumePoint>,
        replaced: Option<ReplacedQueue>,
    ) {
        let play = matches!(mode, QueueMode::Play(_));
        let replacing = replaced.is_some();
        if queue.is_empty() {
            self.add_toast(i18n("No available track found"));
            self.end_startup();
            return;
        }

        self.set_loading(true);
        self.set_playlist_visible(true);

        // Begin the trace
        let now = Instant::now();

        // The tracks are read in a separate thread, and added all at once
        let n_files = queue.len() as u32;
        let receiver = load_tracks(queue);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let mut tracks = Vec::new();
                let mut cur_file: u32 = 0;
                while let Ok(data) = receiver.recv().await {
                    cur_file += 1;
                    win.imp().playlist_view.update_loading(cur_file, n_files);
                    if let Some(data) = data {
                        tracks.push(Track::new(data));
                    }
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
                    win.add_replaced_toast(tracks.len(), replaced);
                } else if tracks.is_empty() {
                    win.add_toast(i18n("No tracks found"));
                }

                if tracks.is_empty() {
                    // Nothing to add
                } else if let Some(player) = win.player() {
                    let queue = player.queue();
                    let was_empty = queue.is_empty();
                    let first_added = queue.n_tracks();

                    // Bulk add to avoid hammering the UI with list model updates
                    queue.add_tracks(&tracks);

                    // Store the current state of the queue; restoring
                    // doesn't change it, and saving it would drop the
                    // tracks that couldn't be read, e.g. on another drive
                    if mode != QueueMode::Restore {
                        win.store_queue();
                    }

                    debug!(
                        "Queue was empty: {}, new size: {}",
                        was_empty,
                        queue.n_tracks()
                    );
                    if let QueueMode::Play(start) = mode {
                        player.skip_to(first_added + start);
                        player.play();
                    } else if was_empty {
                        let resume_at = resume.and_then(|r| {
                            let uris: Vec<String> = tracks.iter().map(Track::uri).collect();
                            Some((r.find_track(&uris)?, r.position))
                        });
                        match resume_at {
                            Some((pos, position)) => {
                                // Shuffling isn't restored, so the order
                                // is the one the tracks were added in
                                debug_assert!(!queue.is_shuffled());
                                player.skip_to(pos);
                                player.resume_at(position.into());
                            }
                            None => player.skip_to(0),
                        }
                    }
                    if mode == QueueMode::Add {
                        // Show where the album went, even if the
                        // queue was closed while adding it
                        win.set_playlist_visible(true);
                    } else {
                        win.set_library_visible(false);
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
                            win.add_skip_to_toast(
                                i18n("Added a new track"),
                                i18n("Play"),
                                &tracks[0],
                            );
                        }
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
            }
        ));
    }

    // Adds the audio files, and the ones in the folders, to the queue;
    // looking into the folders takes a while, so it's done in a separate
    // thread
    fn add_files_to_queue(&self, files: Vec<gio::File>) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                match gio::spawn_blocking(move || audio_files(files)).await {
                    Ok(queue) => win.queue_tracks(queue, QueueMode::Show),
                    Err(_) => win.end_startup(),
                }
            }
        ));
    }

    // Plays the audio files, and the ones in the folders, in place of the
    // tracks of the queue, like an album's Play button; nothing is replaced
    // until some are found
    fn play_dropped_files(&self, files: Vec<gio::File>) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Ok(queue) = gio::spawn_blocking(move || audio_files(files)).await else {
                    return;
                };
                if queue.is_empty() {
                    win.add_toast(i18n("No available track found"));
                    return;
                }

                let replaced = win.replaced_queue();
                win.select_queue_to_play();
                win.clear_queue();
                win.show_playlist_view();
                win.load_into_queue(queue, QueueMode::Play(0), None, None, Some(replaced));
            }
        ));
    }

    fn add_replaced_toast(&self, n_tracks: usize, replaced: ReplacedQueue) {
        let msg = if n_tracks == 0 {
            i18n("No tracks found")
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
        self.add_toast_with_button(msg, i18n("Undo"), move |win| {
            win.undo_replace(&replaced);
        });
    }

    // Reads the tags of the tracks in the queue and in the album shown in
    // the sidebar again when their files changed, e.g. when a scan of the
    // library finds new tags; the tracks are updated in place, so the
    // lists keep their order and the current track keeps playing
    pub(super) fn reload_changed_tracks(&self, uris: &[String]) {
        let Some(player) = self.player() else {
            return;
        };
        let shown: Vec<Track> = player
            .queue()
            .model()
            .iter::<Track>()
            .flatten()
            .chain(self.imp().album_tracks.tracks())
            .collect();

        // The cover of their album might have changed too, and the other
        // tracks of the album share it, so they are read again with them
        let covers: HashSet<String> = shown
            .iter()
            .filter(|track| uris.contains(&track.uri()))
            .filter_map(Track::cover_uuid)
            .collect();
        let tracks: Vec<Track> = shown
            .into_iter()
            .filter(|track| {
                uris.contains(&track.uri())
                    || track
                        .cover_uuid()
                        .is_some_and(|uuid| covers.contains(&uuid))
            })
            .collect();
        if tracks.is_empty() {
            return;
        }
        {
            let mut cache = CoverCache::global().lock().unwrap();
            for uuid in &covers {
                cache.forget(uuid);
            }
        }

        let receiver = load_tracks(tracks.iter().map(Track::file).collect());
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                for track in tracks {
                    let Ok(data) = receiver.recv().await else {
                        break;
                    };
                    // A file that can't be read anymore keeps its old tags
                    let Some(data) = data else {
                        continue;
                    };
                    track.update(data);

                    let Some(player) = win.player() else {
                        return;
                    };
                    player.queue().track_changed(&track);
                    win.imp().album_tracks.track_changed(&track);
                    if player.state().track().as_ref() == Some(&track) {
                        player.current_track_changed();
                        win.update_title(Some(&track));
                        win.update_style(Some(&track));
                    }
                }
            }
        ));
    }

    pub fn open_files(&self, files: &[gio::File]) {
        if files.is_empty() {
            self.add_toast(i18n("Unable to access files"));
            self.end_startup();
            return;
        }

        // At startup, the selected playlist isn't loaded, and the files
        // take the place of the queue instead
        if self.imp().starting.get() {
            self.select_queue_to_play();
        }

        self.add_files_to_queue(files.to_vec());
    }

    pub(super) fn setup_drop_target(&self) {
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
            move |_, value, x, y| {
                if let Ok(file_list) = value.get::<gdk::FileList>() {
                    if file_list.files().is_empty() {
                        win.add_toast(i18n("Unable to access dropped files"));
                        return false;
                    }

                    // Dropped on the zone that plays them, or on the one
                    // that adds them
                    if win.imp().drag_overlay.in_play_zone(x, y) {
                        win.play_dropped_files(file_list.files());
                    } else {
                        win.add_files_to_queue(file_list.files());
                    }
                    return true;
                }

                false
            }
        ));

        self.imp().drag_overlay.set_drop_target(Some(&drop_target));
    }
}

// The audio files, and the ones in the folders and their subfolders. The
// files and folders given can be symbolic links, which were picked or
// dropped on purpose; the ones inside the folders aren't followed, as
// they could make a loop
fn audio_files(files: Vec<gio::File>) -> Vec<gio::File> {
    let mut queue = Vec::new();

    for file in files {
        if let Ok(info) = file.query_info(
            "standard::name,standard::display-name,standard::type,standard::content-type",
            gio::FileQueryInfoFlags::NONE,
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
                    queue.extend(utils::load_files_from_folder(&file, true));
                }
                _ => (),
            }
        }
    }

    queue
}
