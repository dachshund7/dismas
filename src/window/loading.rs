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

    // Restores the queue of the last session, if the user wants it
    pub fn restore_queue_on_startup(&self) {
        if self.imp().settings.boolean("restore-queue")
            && utils::has_cached_queue()
            && self.queue_is_empty()
        {
            self.restore_queue();
        } else {
            self.end_startup();
        }
    }

    fn restore_queue(&self) {
        // The saved queue is empty after clearing the queue
        match utils::load_cached_queue() {
            Some(tracks) if !tracks.is_empty() => self.queue_tracks(tracks, QueueMode::Restore),
            _ => self.end_startup(),
        }
    }

    // Adds the files to the end of the queue
    pub(super) fn queue_tracks(&self, queue: Vec<gio::File>, mode: QueueMode) {
        let play = matches!(mode, QueueMode::Play(_));
        if queue.is_empty() {
            self.add_toast(i18n("No available track found"));
            self.end_startup();
            return;
        }

        // Disable actions on the queue; loading is "atomic"
        self.action_set_enabled("queue.clear", false);
        self.imp().album_tracks.set_adding_enabled(false);
        self.imp().library_view.set_adding_enabled(false);

        self.set_playlist_visible(true);
        self.imp().playlist_view.begin_loading();

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

                // Re-enable the actions
                win.action_set_enabled("queue.clear", true);
                win.imp().album_tracks.set_adding_enabled(true);
                win.imp().library_view.set_adding_enabled(true);

                if tracks.is_empty() {
                    win.add_toast(i18n("No tracks found"));
                } else if let Some(player) = win.player() {
                    let queue = player.queue();
                    let was_empty = queue.is_empty();
                    let first_added = queue.n_tracks();

                    win.imp().playlist_view.end_loading();

                    // Bulk add to avoid hammering the UI with list model updates
                    queue.add_tracks(&tracks);

                    // Store the current state of the queue
                    utils::store_queue(queue);

                    debug!(
                        "Queue was empty: {}, new size: {}",
                        was_empty,
                        queue.n_tracks()
                    );
                    if let QueueMode::Play(start) = mode {
                        player.skip_to(first_added + start);
                        player.play();
                    } else if was_empty {
                        player.skip_to(0);
                    }
                    if mode == QueueMode::Add {
                        // Show where the album went, even if the
                        // queue was closed while adding it
                        win.set_playlist_visible(true);
                    } else {
                        win.set_library_visible(false);
                    }

                    // Restoring the queue at startup is quiet, and
                    // doesn't start playing
                    if mode == QueueMode::Restore {
                        // Nothing to announce
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

    pub fn open_files(&self, files: &[gio::File]) {
        if files.is_empty() {
            self.add_toast(i18n("Unable to access files"));
            self.end_startup();
            return;
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
            move |_, value, _, _| {
                if let Ok(file_list) = value.get::<gdk::FileList>() {
                    if file_list.files().is_empty() {
                        win.add_toast(i18n("Unable to access dropped files"));
                        return false;
                    }

                    win.add_files_to_queue(file_list.files());
                    return true;
                }

                false
            }
        ));

        self.imp().drag_overlay.set_drop_target(Some(&drop_target));
    }
}

// The audio files, and the ones in the folders and their subfolders
fn audio_files(files: Vec<gio::File>) -> Vec<gio::File> {
    let mut queue = Vec::new();

    for file in files {
        if let Ok(info) = file.query_info(
            "standard::name,standard::display-name,standard::type,standard::content-type",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
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
