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
                self.load_into_queue(files, QueueMode::Restore, None, resume, None, None);
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
        self.load_into_queue(queue, mode, None, None, None, None);
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
    // can put them back. With `insert_before`, they go before that track,
    // or at the end if it was removed meanwhile
    fn load_into_queue(
        &self,
        queue: Vec<gio::File>,
        mode: QueueMode,
        switch_to: Option<String>,
        resume: Option<ResumePoint>,
        replaced: Option<ReplacedQueue>,
        insert_before: Option<Track>,
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
                    queue.insert_tracks(&tracks, insert_before.as_ref());
                    // The list keeps the rows it shows in place, so tracks put
                    // above the first of them would be out of its view
                    if insert_before.is_some() {
                        crate::queue_row::reveal_track(
                            &win.imp().playlist_view.queue_view(),
                            &tracks[0],
                        );
                    }

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

    // Adds the audio files, and the ones in the folders, to the queue, at
    // the end or before a track; looking into the folders takes a while, so
    // it's done in a separate thread
    fn add_files_to_queue(&self, files: Vec<gio::File>, mode: QueueMode, before: Option<Track>) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                match gio::spawn_blocking(move || audio_files(files)).await {
                    Ok(queue) => win.load_into_queue(queue, mode, None, None, None, before),
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
                win.load_into_queue(queue, QueueMode::Play(0), None, None, Some(replaced), None);
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

        self.add_files_to_queue(files.to_vec(), QueueMode::Show, None);
    }

    // A single target takes the files dropped anywhere on the window, and
    // decides from where they are what they do: on X11 the widgets aren't
    // told that the drag left them once it's dropped, so a second target
    // that took the drag without getting the drop would keep it, and turn
    // the next one down
    pub(super) fn setup_drop_target(&self) {
        let drop_target = gtk::DropTarget::builder()
            .name("file-drop-target")
            .actions(gdk::DragAction::COPY)
            .formats(&gdk::ContentFormats::for_type(gdk::FileList::static_type()))
            .build();

        let show = clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_: &gtk::DropTarget, x: f64, y: f64| {
                win.show_file_drop(x, y);
                gdk::DragAction::COPY
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
            move |_, value, x, y| {
                let Ok(file_list) = value.get::<gdk::FileList>() else {
                    return false;
                };
                if file_list.files().is_empty() {
                    win.add_toast(i18n("Unable to access dropped files"));
                    return false;
                }

                let files = file_list.files();
                match win.file_drop_at(x, y) {
                    FileDrop::Zones if win.imp().drag_overlay.in_play_zone(x, y) => {
                        win.play_dropped_files(files);
                    }
                    FileDrop::Zones => win.add_files_to_queue(files, QueueMode::Show, None),
                    // Browsing the albums goes on
                    FileDrop::Place(row, above) => {
                        let before = row.track().and_then(|target| {
                            if above {
                                Some(target)
                            } else {
                                win.player()?.queue().next_in_order(&target)
                            }
                        });
                        win.add_files_to_queue(files, QueueMode::Add, before);
                    }
                    FileDrop::AboveWork(row) => {
                        win.add_files_to_queue(files, QueueMode::Add, row.track());
                    }
                    FileDrop::End => win.add_files_to_queue(files, QueueMode::Add, None),
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

    // What files dropped at a point of the window would do; the point is
    // relative to the drag overlay, which has the whole window. While the
    // queue is shuffled, there is no place to put them in
    fn file_drop_at(&self, x: f64, y: f64) -> FileDrop {
        let imp = self.imp();
        if self.queue_shuffled() {
            return FileDrop::Zones;
        }
        let Some(picked) = imp.drag_overlay.pick(x, y, gtk::PickFlags::DEFAULT) else {
            return FileDrop::Zones;
        };
        let pane = imp.playlist_pane.upcast_ref::<gtk::Widget>();
        if picked != *pane && !picked.is_ancestor(pane) {
            return FileDrop::Zones;
        }
        self.queue_drop_place(&picked, y).unwrap_or(FileDrop::End)
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

    // Shows what files dropped at a point would do: a line between the
    // tracks of the queue where they'd go, the outline of the pane when
    // they'd go at the end, or the zones
    fn show_file_drop(&self, x: f64, y: f64) {
        let imp = self.imp();
        let drop = self.file_drop_at(x, y);

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
    // dropped on the pane go in between its tracks, or all of the window
    // while shuffled. Next to the sidebar of the collapsed split view, the
    // main area is only drawn moved aside, so the zones are laid out where
    // it shows rather than over it
    fn zones_area(&self) -> (f32, f32, gtk::Widget) {
        let imp = self.imp();
        let overlay = &*imp.drag_overlay;
        let width = overlay.width() as f32;
        let whole = (0.0, width, imp.toast_overlay.get().upcast());
        if self.queue_shuffled() || !imp.playlist_pane.is_mapped() {
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
    fn end_file_drop(&self) {
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
