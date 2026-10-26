// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    pub(super) fn setup_playlist(&self) {
        let imp = self.imp();

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, item| {
                let row = QueueRow::default();
                let list_item = item.downcast_ref::<gtk::ListItem>().unwrap();
                // Rows slide to their new places when tracks are moved
                let bin = SlideBin::new(&row);
                list_item.set_child(Some(&bin));
                let rows = &mut *win.imp().queue_rows.borrow_mut();
                rows.retain(|row| row.upgrade().is_some());
                rows.push(bin.downgrade());
                // A row reused for another track shows it in its place
                row.connect_track_notify(clone!(
                    #[weak]
                    bin,
                    move |_| bin.stop_sliding()
                ));

                row.connect_notify_local(
                    Some("selected"),
                    clone!(
                        #[weak]
                        win,
                        move |_, _| {
                            win.update_selected_count();
                        }
                    ),
                );

                row.connect_menu_requested(clone!(
                    #[weak]
                    win,
                    move |row, x, y| win.show_track_menu(row, x, y, true)
                ));
                row.connect_select_requested(clone!(
                    #[weak]
                    win,
                    move |row| {
                        if let Some(track) = row.track() {
                            win.toggle_queue_track(&track);
                        }
                    }
                ));
                row.connect_range_select_requested(clone!(
                    #[weak]
                    win,
                    move |row| {
                        if let Some(track) = row.track() {
                            win.select_queue_range(&track);
                        }
                    }
                ));

                win.bind_property("queue-selection", &row, "selection-mode")
                    .sync_create()
                    .build();

                // While a search hides tracks, the rows around a place in
                // the list aren't the tracks around it in the queue, so the
                // tracks can't be moved, and dropped ones go at the end
                win.imp()
                    .queue_filter
                    .bind_property("filtering", &row, "reorderable")
                    .invert_boolean()
                    .sync_create()
                    .build();
                row.connect_move_requested(clone!(
                    #[weak]
                    win,
                    move |row, track, above| {
                        if let Some(target) = row.track() {
                            win.move_queue_track(track, &target, above);
                        }
                    }
                ));

                row.bind_list_item(list_item);
            }
        ));
        imp.playlist_view
            .queue_view()
            .set_factory(Some(&factory.upcast::<gtk::ListItemFactory>()));
        imp.playlist_view
            .queue_view()
            .set_header_factory(Some(&work_header_factory(
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |start, end, ctrl| win.toggle_queue_work(start, end, ctrl)
                ),
                Some(clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |track: &Track, position| {
                        let target = win
                            .imp()
                            .playlist_view
                            .queue_view()
                            .model()
                            .and_then(|model| model.item(position))
                            .and_downcast::<Track>();
                        if let Some(target) = target {
                            win.move_queue_track(track, &target, true);
                        }
                    }
                )),
            )));

        let player = self.player();
        let queue = player.queue();

        let filter = imp.queue_filter.clone();
        let filter_model =
            gtk::FilterListModel::new(Some(queue.model().clone()), Some(filter.clone()));
        // Search results keep the order of the queue
        let sections = WorkSectionModel::new(&filter_model);
        let selection = gtk::NoSelection::new(Some(sections));
        imp.playlist_view
            .queue_view()
            .set_model(Some(selection.upcast_ref::<gtk::SelectionModel>()));
        imp.playlist_view.queue_view().connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[weak]
            selection,
            move |_, pos| {
                let track = selection
                    .upcast::<gio::ListModel>()
                    .item(pos)
                    .unwrap()
                    .downcast::<Track>()
                    .unwrap();

                let player = win.player();
                let queue = player.queue();
                // Where it is in the queue, rather than in a search
                if let Some(real_pos) = queue.position(&track) {
                    if win.queue_selection() {
                        win.toggle_queue_track(&track);
                        return;
                    }
                    if let Some(reason) = track.missing_reason() {
                        let title = track.title();
                        win.add_toast(match reason {
                            Missing::NotFound => i18n_k(
                                // Translators: `{track}` must be left
                                // unmodified; it is expanded to the
                                // title of a track
                                "“{track}” wasn't found",
                                &[("track", &title)],
                            ),
                            Missing::Unreadable => i18n_k(
                                // Translators: `{track}` must be left
                                // unmodified; it is expanded to the
                                // title of a track
                                "Unable to play “{track}”",
                                &[("track", &title)],
                            ),
                        });
                        return;
                    }
                    if queue.current_track_index() != Some(real_pos) {
                        player.skip_to(real_pos);
                        player.play();
                    } else if player.state().playing() {
                        // Activating the playing track pauses it
                        player.pause();
                        return;
                    } else {
                        player.play();
                    }
                    // Playing a track brings up the player in place
                    // of the album grid
                    win.set_library_visible(false);
                }
            }
        ));

        imp.queue_filtermodel
            .replace(Some(filter_model.clone().upcast::<gio::ListModel>()));
        // What the search found decides whether its tracks can be played,
        // and whether the pane says it found nothing. The queue's own
        // changes count too: tracks added that the search hides don't
        // change what it found, but the queue isn't empty any more. The
        // filter model takes the queue's changes first, as it connected
        // to them first
        filter_model.connect_items_changed(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _, _, _| win.update_search_results()
        ));
        let items_changed_id = queue.model().connect_items_changed(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _, _, _| win.update_search_results()
        ));
        self.disconnect_on_close(queue.model(), items_changed_id);
        filter.connect_filtering_notify(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.update_search_results()
        ));

        imp.playlist_view
            .queue_searchentry()
            .bind_property("text", &filter, "search")
            .sync_create()
            .build();
        imp.playlist_view
            .queue_searchentry()
            .connect_search_changed(clone!(
                #[weak(rename_to = win)]
                self,
                move |_| {
                    if let Some(adjustment) = win.imp().playlist_view.queue_view().vadjustment() {
                        adjustment.set_value(0.0);
                    }
                }
            ));

        // Esc cancels dragging a track, or albums from the grid. The drag
        // doesn't always get the key itself (on X11 it reaches the window),
        // so it's caught before the panes and the window, which would leave
        // selection mode or hide the playlist pane, leaving the track with
        // nowhere to be dropped
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == gdk::Key::Escape && (win.cancel_queue_drag() || win.cancel_albums_drag())
                {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        self.add_controller(keys);
    }

    // The albums dragged from the grid don't go anywhere, and the lines
    // that showed where they'd go are hidden
    fn cancel_albums_drag(&self) -> bool {
        if !self.imp().library_view.cancel_drag() {
            return false;
        }
        self.end_file_drop();
        true
    }

    fn cancel_queue_drag(&self) -> bool {
        let rows: Vec<QueueRow> = self
            .imp()
            .queue_rows
            .borrow()
            .iter()
            .filter_map(glib::WeakRef::upgrade)
            .filter_map(|bin| bin.child().and_downcast::<QueueRow>())
            .collect();
        if !rows.iter().any(QueueRow::cancel_drag) {
            return false;
        }
        for row in &rows {
            row.hide_drop_position();
        }
        true
    }

    pub(super) fn reset_queue(&self) {
        // The sidebar shows the tracks of the playlist replacing these
        if self.imp().pending_playlist.borrow().is_none() {
            self.set_playlist_visible(false);
        }
        self.set_queue_selection(false);
        self.update_style(None);
    }

    // Empties the selected playlist, e.g. for tracks to play in place of
    // those of the queue; the order it had before it was shuffled goes too
    pub(super) fn clear_queue(&self) {
        self.player().clear_queue();

        // So that the queue isn't restored at the next startup
        self.store_queue();
        playlists::drop_unshuffled(&self.selected_playlist());
        self.sync_queue_shuffled();
    }

    pub(super) fn queue_is_empty(&self) -> bool {
        self.player().queue().is_empty()
    }

    // Adds up the tracks after the current one again, when the tracks or
    // their order change, or another track is current
    pub(super) fn update_queue_time(&self) {
        let player = self.player();
        let queue = player.queue();
        let after_current: u64 = queue.current_track_index().map_or(0, |current| {
            (current + 1..queue.n_tracks())
                .filter_map(|pos| queue.track_at(pos))
                .map(|track| track.duration())
                .sum()
        });
        self.imp().queue_time_after_current.set(after_current);
        self.show_queue_time();
    }

    // Shows the time left in the queue, counting only what's left of the
    // current track, so it's called at each position too
    pub(super) fn show_queue_time(&self) {
        let player = self.player();
        let queue = player.queue();
        if let Some(current) = queue.current() {
            let state = player.state();
            let elapsed = if state.track().as_ref() == Some(&current) {
                state.position()
            } else {
                0
            };
            let remaining_time = self.imp().queue_time_after_current.get()
                + current.duration().saturating_sub(elapsed);
            // Rounded up, so the last minute isn't "0 minutes remaining"
            let remaining_min = u32::try_from(remaining_time.div_ceil(60)).unwrap_or(u32::MAX);
            let remaining_hrs = remaining_min / 60;

            let remaining_str = if remaining_hrs > 0 {
                // Translators: the `{}` must be left unmodified, and
                // it will be replaced by the number of minutes remaining
                // in the string "N hours M minutes remaining"
                let minutes = ni18n_f(
                    "{} minute",
                    "{} minutes",
                    remaining_min % 60,
                    &[&(remaining_min % 60).to_string()],
                );

                // Translators: `{hours}` and `{minutes}` must be left
                // unmodified, and they will be replaced by number of
                // hours and by the translated number of minutes
                // remaining, respectively
                ni18n_k(
                    "{hours} hour {minutes} remaining",
                    "{hours} hours {minutes} remaining",
                    remaining_hrs,
                    &[("hours", &remaining_hrs.to_string()), ("minutes", &minutes)],
                )
            } else {
                ni18n_f(
                    // Translators: the '{}' must be left unmodified, and
                    // it will be replaced by the number of minutes remaining
                    // in the queue
                    "{} minute remaining",
                    "{} minutes remaining",
                    remaining_min,
                    &[&remaining_min.to_string()],
                )
            };

            self.imp()
                .playlist_view
                .queue_length_label()
                .set_label(&remaining_str);
            self.imp()
                .playlist_view
                .queue_length_label()
                .set_visible(true);
        } else {
            self.imp()
                .playlist_view
                .queue_length_label()
                .set_visible(false);
        }
    }

    // Only the last track to go to is scrolled to, e.g. skipping several
    pub(super) fn scroll_queue_to_track(&self) {
        let imp = self.imp();
        if let Some(tick) = imp.queue_scroll_tick.take() {
            tick.remove();
        }
        let Some(track) = self.player().queue().current() else {
            return;
        };
        let view = imp.playlist_view.queue_view();
        let tick = crate::queue_row::reveal_track(&view, &track);
        imp.queue_scroll_tick.replace(Some(tick));
    }

    // Closes the search of the queue, showing all its tracks again; the
    // text goes right away, so that the list has them all at once
    pub(super) fn close_queue_search(&self) {
        self.imp().playlist_view.queue_searchentry().set_text("");
        self.set_queue_search(false);
    }

    // Plays the tracks the search found, in their order, in place of
    // those of the queue, with an Undo, like a smart playlist; the ones
    // whose files are missing are left out, as they can't play
    pub(super) fn play_search_results(&self) {
        let imp = self.imp();
        if !self.queue_filtering() || imp.loads.get() > 0 {
            return;
        }
        // By position: the filter model's item type is GObject, which
        // `iter::<Track>()` would refuse, though its items are tracks
        let files: Vec<gio::File> = imp
            .queue_filtermodel
            .borrow()
            .iter()
            .flat_map(|model| (0..model.n_items()).filter_map(|pos| model.item(pos)))
            .filter_map(|item| item.downcast::<Track>().ok())
            .filter(|track| !track.missing())
            .map(|track| track.file())
            .collect();
        if files.is_empty() {
            return;
        }
        self.play_in_place_of_queue(files);
    }

    fn update_search_results(&self) {
        self.update_queue_actions();
        // While the queue is emptied to be replaced, the search finds
        // nothing in it, which isn't worth saying
        let found_nothing = self.queue_filtering()
            && !self.queue_is_empty()
            && self
                .imp()
                .queue_filtermodel
                .borrow()
                .as_ref()
                .is_some_and(|model| model.n_items() == 0);
        self.imp().playlist_view.set_no_results(found_nothing);
    }

    // Whether the search of the queue hides some of its tracks
    pub(super) fn queue_filtering(&self) -> bool {
        self.imp().queue_filter.filtering()
    }

    // Shows the playing track in the queue, in the middle of the list,
    // where it flashes, like going to the album of a track; a search that
    // hides it is cleared
    pub(super) fn show_playing_track(&self) {
        let Some(track) = self.player().state().track() else {
            return;
        };
        let view = self.imp().playlist_view.queue_view();
        if crate::queue_row::track_position(&view, &track).is_none() && self.queue_search() {
            self.close_queue_search();
        }
        self.flash_queue_track(&track);
    }

    // Shows a track that a search found among all the tracks of the queue,
    // closing the search, e.g. to add or move tracks around it
    pub(super) fn show_in_playlist(&self, track: &Track) {
        self.close_queue_search();
        self.flash_queue_track(track);
    }

    // Scrolls the queue to a track, which flashes in the middle of the list
    fn flash_queue_track(&self, track: &Track) {
        let imp = self.imp();
        let view = imp.playlist_view.queue_view();
        let Some(pos) = crate::queue_row::track_position(&view, track) else {
            return;
        };

        self.show_playlist_view();
        self.set_playlist_visible(true);
        // A track change stops the glide, and scrolls to the new track
        if let Some(tick) = imp.queue_scroll_tick.take() {
            tick.remove();
        }
        let tick = crate::queue_row::flash_track(&view, pos, track);
        imp.queue_scroll_tick.replace(Some(tick));
    }

    // Moves a track above or below another one, dropped on its row. In
    // selection mode, dragging a selected track moves all the selected
    // ones, together and in the order they had
    fn move_queue_track(&self, track: &Track, target: &Track, above: bool) {
        let player = self.player();
        let queue = player.queue();
        let tracks = if self.queue_selection() && track.selected() {
            queue.selected_tracks()
        } else {
            vec![track.clone()]
        };
        let before = if above {
            Some(target.clone())
        } else {
            queue.track_after(target)
        };

        let shown = self.shown_queue_rows();
        let old_order: Vec<Track> = queue.model().iter::<Track>().flatten().collect();
        player.move_tracks(&tracks, before.as_ref());
        self.store_queue();

        // Dragging works in the queue, so the focus goes to the dropped
        // track; not when the drag starts, as focusing a row that's cut off
        // scrolls it from under the pointer
        let queue_view = self.imp().playlist_view.queue_view();
        if let Some(model) = queue_view.model()
            && let Some(pos) = (0..model.n_items())
                .find(|&i| model.item(i).and_downcast_ref::<Track>() == Some(track))
        {
            queue_view.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
            queue_view.grab_focus();
        }

        // The rows get their new places when the list is laid out, before
        // it's drawn, so they can still be drawn in their old places first
        let Some(clock) = self.imp().playlist_view.queue_view().frame_clock() else {
            return;
        };
        let handler = Rc::new(RefCell::new(None));
        let id = clock.connect_layout(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            handler,
            move |clock| {
                if let Some(id) = handler.take() {
                    clock.disconnect(id);
                }
                win.slide_queue_rows(&shown, &old_order);
            }
        ));
        handler.replace(Some(id));
    }

    // The tracks of the rows in view, and where the rows are drawn
    fn shown_queue_rows(&self) -> Vec<(Track, f64)> {
        let view = self.imp().playlist_view.queue_view();
        self.mapped_queue_rows()
            .iter()
            .filter_map(|bin| {
                let track = bin.child().and_downcast::<QueueRow>()?.track()?;
                Some((track, bin.shown_y(&view)?))
            })
            .collect()
    }

    pub(super) fn mapped_queue_rows(&self) -> Vec<SlideBin> {
        self.imp()
            .queue_rows
            .borrow()
            .iter()
            .filter_map(glib::WeakRef::upgrade)
            .filter(WidgetExt::is_mapped)
            .collect()
    }

    // Slides the rows from where their tracks were shown to their new
    // places. Rows of tracks that were out of view come from above or
    // below it, as far as their tracks moved
    fn slide_queue_rows(&self, shown: &[(Track, f64)], old_order: &[Track]) {
        let player = self.player();
        let queue = player.queue();
        let view = self.imp().playlist_view.queue_view();
        let view_height = f64::from(view.height());
        for bin in self.mapped_queue_rows() {
            let Some(track) = bin
                .child()
                .and_downcast::<QueueRow>()
                .and_then(|r| r.track())
            else {
                continue;
            };
            if let Some((_, y)) = shown.iter().find(|(t, _)| *t == track) {
                bin.slide_from(*y, &view);
                continue;
            }
            let Some(y) = bin.shown_y(&view) else {
                continue;
            };
            let (Some(old_pos), Some(new_pos)) = (
                old_order.iter().position(|t| *t == track),
                queue.position(&track),
            ) else {
                continue;
            };
            let height = f64::from(bin.height());
            let moved_by = (old_pos as f64 - f64::from(new_pos)) * height;
            bin.slide_from((y + moved_by).clamp(-height, view_height), &view);
        }
    }

    // Removes tracks from the queue, in a single change
    pub fn remove_tracks(&self, tracks: &[Track]) {
        self.player().remove_tracks(tracks);
        self.update_selected_count();
    }
}
