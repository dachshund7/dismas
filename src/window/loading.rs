// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // Called once the playlist of the last session, or the files the
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

    // Restores the playlist of the last session, if the user wants it
    pub fn restore_playlist_on_startup(&self) {
        if self.imp().settings.boolean("restore-playlist")
            && utils::has_cached_playlist()
            && self.queue_is_empty()
        {
            self.restore_playlist();
        } else {
            self.end_startup();
        }
    }

    fn restore_playlist(&self) {
        // The saved playlist is empty after clearing the playlist
        match utils::load_cached_tracks() {
            Some(tracks) if !tracks.is_empty() => self.queue_tracks(tracks, QueueMode::Restore),
            _ => self.end_startup(),
        }
    }

    // Adds the files to the end of the playlist
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

        // Turn the list of files into tracks one at a time into the main loop
        let n_files = queue.len() as u32;

        let mut files = queue.into_iter();
        let mut tracks = Vec::new();
        let mut cur_file: u32 = 0;
        let mut duplicates: u32 = 0;

        glib::idle_add_local(clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                files
                    .next()
                    .map(|f| {
                        win.imp().playlist_view.update_loading(cur_file, n_files);
                        if let Ok(s) = Track::from_uri(f.uri().as_str())
                            && let Some(player) = win.player()
                        {
                            let queue = player.queue();
                            if queue.contains(&s) {
                                duplicates += 1;
                            } else {
                                tracks.push(s);
                                cur_file += 1;
                            }
                        }
                    })
                    .map(|_| glib::ControlFlow::Continue)
                    .unwrap_or_else(|| {
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
                            if duplicates == 0 {
                                win.add_toast(i18n("No tracks found"));
                            }
                        } else if let Some(player) = win.player() {
                            let queue = player.queue();
                            let was_empty = queue.is_empty();
                            let first_added = queue.n_tracks();

                            win.imp().playlist_view.end_loading();

                            // Bulk add to avoid hammering the UI with list model updates
                            queue.add_tracks(&tracks);

                            // Store the current state of the playlist
                            utils::store_playlist(queue);

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
                                // playlist was closed while adding it
                                win.set_playlist_visible(true);
                            } else {
                                win.set_library_visible(false);
                            }

                            // Restoring the playlist at startup is quiet, and
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
                                        queue.n_tracks() - 1,
                                    );
                                }
                            } else {
                                let msg = ni18n_f(
                                    // Translators: the `{}` must be left unmodified;
                                    // it will be expanded to the number of tracks added
                                    // to the playlist
                                    "Added one track",
                                    "Added {} tracks",
                                    tracks.len() as u32,
                                    &[&tracks.len().to_string()],
                                );

                                win.add_toast(msg);
                            }
                        }

                        win.end_startup();
                        glib::ControlFlow::Break
                    })
            }
        ));
    }

    fn add_files_to_queue(&self, model: &gio::ListModel) {
        let mut queue: Vec<gio::File> = vec![];

        for pos in 0..model.n_items() {
            let file = model.item(pos).unwrap().downcast::<gio::File>().unwrap();

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
                        let files = utils::load_files_from_folder(&file, true);
                        queue.extend(files);
                    }
                    _ => (),
                }
            }
        }

        self.queue_tracks(queue, QueueMode::Show);
    }

    pub fn open_files(&self, files: &[gio::File]) {
        if files.is_empty() {
            self.add_toast(i18n("Unable to access files"));
            self.end_startup();
            return;
        }

        let model = gio::ListStore::new::<gio::File>();
        for f in files {
            model.append(f);
        }
        self.add_files_to_queue(model.upcast_ref::<gio::ListModel>());
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

                    let model = gio::ListStore::new::<gio::File>();
                    for f in file_list.files() {
                        model.append(&f);
                    }
                    win.add_files_to_queue(model.upcast_ref::<gio::ListModel>());
                    return true;
                }

                false
            }
        ));

        self.imp().drag_overlay.set_drop_target(Some(&drop_target));
    }
}
