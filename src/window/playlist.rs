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
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |widget: &gtk::Widget, x, y, track: &Track| {
                        win.show_work_menu(widget, x, y, track);
                    }
                ),
                // While a search hides tracks, they can't be moved, as with
                // the rows
                Some((
                    clone!(
                        #[weak(rename_to = win)]
                        self,
                        #[upgrade_or]
                        false,
                        move || !win.queue_filtering()
                    ),
                    clone!(
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
                    ),
                )),
                // The queue mixes albums, so it names no discs
                |_: &Track, _| None,
            )));

        let player = self.player();
        let queue = player.queue();

        let filter = &imp.queue_filter;
        let filter_model =
            gtk::FilterListModel::new(Some(queue.model().clone()), Some(filter.clone()));
        // Search results keep the order of the queue
        let sections = WorkSectionModel::new(&filter_model);
        let selection = gtk::NoSelection::new(Some(sections));
        imp.playlist_view
            .queue_view()
            .set_model(Some(selection.upcast_ref::<gtk::SelectionModel>()));
        utils::setup_menu_key(&imp.playlist_view.queue_view(), QueueRow::request_menu);
        self.setup_move_keys();
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
            .bind_property("text", filter, "search")
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
        // Without a track, there are no lyrics to show
        if self.lyrics_on_pane() {
            self.show_playlist_view();
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

    // The same once the queue's tracks are done changing: a scan that
    // retags many of them changes them one at a time, and adding up all the
    // tracks after the current one at each would stall a long queue
    pub(super) fn update_queue_time_later(&self) {
        if self.imp().queue_time_idle.replace(true) {
            return;
        }
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = win)]
            self,
            move || {
                win.imp().queue_time_idle.set(false);
                win.update_queue_time();
            }
        ));
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

    pub(super) fn scroll_queue_to_track(&self) {
        match self.player().queue().current() {
            Some(track) => self.reveal_queue_track(&track),
            None => self.set_queue_scroll(None),
        }
    }

    // Shows a track in the queue once it's laid out, keeping the rows
    // around it in view
    pub(super) fn reveal_queue_track(&self, track: &Track) {
        let view = self.imp().playlist_view.queue_view();
        self.set_queue_scroll(Some(crate::queue_row::reveal_track(&view, track)));
    }

    // Scrolling the queue takes the place of the scrolling before it, so
    // that only the last place to go to is scrolled to, e.g. skipping
    // several tracks, and two don't pull the list each their own way
    fn set_queue_scroll(&self, tick: Option<gtk::TickCallbackId>) {
        if let Some(old) = self.imp().queue_scroll_tick.replace(tick) {
            old.remove();
        }
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
        let Some(results) = self.queue_search_results() else {
            return;
        };
        if self.imp().loads.get() > 0 {
            return;
        }
        let files: Vec<gio::File> = playable(&results).map(|track| track.file()).collect();
        if files.is_empty() {
            return;
        }
        self.play_in_place_of_queue(files);
    }

    // Whether the search found tracks that can play, for its Play button
    pub(super) fn can_play_search_results(&self) -> bool {
        self.queue_search_results()
            .is_some_and(|results| playable(&results).next().is_some())
    }

    // The tracks the search found, in their order, while it hides some of
    // the queue's
    fn queue_search_results(&self) -> Option<gio::ListModel> {
        if !self.queue_filtering() {
            return None;
        }
        self.imp().queue_filtermodel.borrow().clone()
    }

    fn update_search_results(&self) {
        self.update_queue_actions();
        // While the queue is emptied to be replaced, the search finds
        // nothing in it, which isn't worth saying
        let found_nothing = !self.queue_is_empty()
            && self
                .queue_search_results()
                .is_some_and(|results| results.n_items() == 0);
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
        self.set_queue_scroll(Some(crate::queue_row::flash_track(&view, pos, track)));
    }

    // Alt+Up and Alt+Down move the track with the focus up or down a place,
    // as dragging it there would, with the selected tracks if it's one of
    // them
    fn setup_move_keys(&self) {
        let controller = gtk::ShortcutController::new();
        for (trigger, up) in [
            ("<Alt>Up|<Alt>KP_Up", true),
            ("<Alt>Down|<Alt>KP_Down", false),
        ] {
            controller.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::CallbackAction::new(clone!(
                    #[weak(rename_to = win)]
                    self,
                    #[upgrade_or]
                    glib::Propagation::Proceed,
                    move |list, _| {
                        let row = utils::focused_child::<QueueRow>(list);
                        match row.and_then(|row| row.track()) {
                            Some(track) => {
                                win.move_queue_track_by_one(&track, up);
                                glib::Propagation::Stop
                            }
                            None => glib::Propagation::Proceed,
                        }
                    }
                ))),
            ));
        }
        self.imp()
            .playlist_view
            .queue_view()
            .add_controller(controller);
    }

    // Moves a track, or the selected tracks with it, past the track above
    // the first of them or below the last, unless they're at the end of
    // the queue already. While a search filters the queue, the tracks
    // around a row aren't the ones around it in the queue, so they don't
    // move, as they can't be dragged
    fn move_queue_track_by_one(&self, track: &Track, up: bool) {
        if self.queue_filtering() {
            return;
        }
        let player = self.player();
        let queue = player.queue();
        let positions: Vec<u32> = if self.queue_selection() && track.selected() {
            (0..)
                .zip(queue.model().iter::<Track>().flatten())
                .filter(|(_, track)| track.selected())
                .map(|(pos, _)| pos)
                .collect()
        } else {
            queue.position(track).into_iter().collect()
        };
        let target = if up {
            positions
                .iter()
                .min()
                .and_then(|first| first.checked_sub(1))
        } else {
            positions.iter().max().map(|last| last + 1)
        };
        if let Some(target) = target.and_then(|pos| queue.model().item(pos).and_downcast::<Track>())
        {
            self.move_queue_track(track, &target, up);
        }
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

    // Removes tracks from the queue, in a single change, and saves it; the
    // toast's Undo puts them back
    pub fn remove_tracks(&self, tracks: &[Track]) {
        let mut undo = self.take_queue_undo();
        let runs = self.player().remove_tracks(tracks);
        if runs.is_empty() {
            // The toast that's up still puts back what it did
            if !undo.runs.is_empty() {
                self.imp().queue_undo.replace(Some(undo));
            }
            return;
        }
        self.update_selected_count();
        self.store_queue();
        undo.runs.extend(runs);
        self.add_removed_toast(undo);
    }

    // Remove All empties the selected playlist like removing each of its
    // tracks, with the same Undo, which also puts back the order they had
    // before they were shuffled, as emptying it drops that
    pub(super) fn remove_all_tracks(&self) {
        let player = self.player();
        let queue = player.queue();
        if queue.is_empty() {
            return;
        }
        let mut undo = self.take_queue_undo();
        let tracks: Vec<Track> = queue.model().iter::<Track>().flatten().collect();
        let unshuffled = playlists::load_unshuffled(&self.selected_playlist());
        self.clear_queue();
        // Emptying it is what goes up, and this is still the playlist that
        // the tracks go back to
        undo.generation = queue.generation();
        undo.runs.push(RemovedRun::all(tracks));
        undo.unshuffled = undo.unshuffled.or(unshuffled);
        self.add_removed_toast(undo);
    }

    // The tracks removed while the toast about removed tracks is up are put
    // back together, so that removing them one at a time doesn't leave only
    // the last for Undo. It's taken out while tracks are removed, so that
    // the toast that goes doesn't drop it. It keeps how the window is
    // before the tracks go, in case removing them empties the queue: the
    // album grid then takes the place of the player, the pane closes and
    // playback stops
    fn take_queue_undo(&self) -> QueueUndo {
        let player = self.player();
        let generation = player.queue().generation();
        let mut undo = self
            .imp()
            .queue_undo
            .take()
            .filter(|undo| undo.generation == generation)
            .unwrap_or_else(|| QueueUndo {
                generation,
                ..Default::default()
            });
        undo.page = self.page();
        undo.pane = self.playlist_visible();
        undo.resume = player.resume_point().or(undo.resume);
        undo
    }

    fn add_removed_toast(&self, undo: QueueUndo) {
        let n_tracks: usize = undo.runs.iter().map(|run| run.tracks.len()).sum();
        let msg = ni18n_f(
            // Translators: the `{}` must be left unmodified; it will be
            // expanded to the number of tracks removed from the playlist
            "Removed one track",
            "Removed {} tracks",
            n_tracks as u32,
            &[&n_tracks.to_string()],
        );
        let toast = self.add_undo_toast(msg, Self::undo_removal);
        // The tracks are let go of once the toast is gone
        toast.connect_dismissed(clone!(
            #[weak(rename_to = win)]
            self,
            move |toast| {
                let queue_undo = &win.imp().queue_undo;
                let ours = queue_undo
                    .borrow()
                    .as_ref()
                    .is_some_and(|undo| undo.toast.upgrade().as_ref() == Some(toast));
                if ours {
                    queue_undo.take();
                }
            }
        ));
        undo.toast.set(Some(&toast));
        self.imp().queue_undo.replace(Some(undo));
    }

    pub(super) fn dismiss_removed_toast(&self) {
        let toast = self
            .imp()
            .queue_undo
            .borrow()
            .as_ref()
            .and_then(|undo| undo.toast.upgrade());
        // Not while it's borrowed: the toast's handler takes it
        if let Some(toast) = toast {
            toast.dismiss();
        }
    }

    // Puts back the tracks removed while the toast was up. The last removed
    // go back first, so that each run finds the track it goes before, even
    // one that was removed after it. Playback stays where it is, unless
    // removing emptied the queue: then the page, the pane and the track
    // that was playing, paused where it was, come back too
    fn undo_removal(&self) {
        let Some(undo) = self.imp().queue_undo.take() else {
            return;
        };
        let player = self.player();
        let queue = player.queue();
        // Emptied since for other tracks, such as those of another playlist
        if undo.generation != queue.generation() {
            return;
        }

        let was_empty = queue.is_empty();
        let had_current = queue.current().is_some();
        for run in undo.runs.iter().rev() {
            // Tracks removed in selection mode would come back selected
            for track in &run.tracks {
                track.set_selected(false);
            }
            queue.put_back(run);
        }
        // Kept first, so that a crash in between can't lose it; not over
        // the order of the tracks added and shuffled since
        let name = self.selected_playlist();
        if let Some(files) = &undo.unshuffled
            && !playlists::is_shuffled(&name)
        {
            if let Err(e) = playlists::save_unshuffled(&name, files) {
                log::warn!("Unable to keep the order of the playlist '{name}': {e}");
            }
            self.sync_queue_shuffled();
        }
        self.store_queue();
        self.update_selected_count();

        if !had_current {
            let resume = undo
                .resume
                .and_then(|(track, position)| Some((queue.position(&track)?, position)));
            match resume {
                Some((pos, position)) if queue.playable_track(pos).is_some() => {
                    player.skip_to(pos);
                    player.resume_at(position);
                }
                _ => player.skip_to_first(),
            }
        }
        // The album grid took the place of the player, and the pane closed;
        // a page gone to since stays
        if was_empty {
            if undo.page == "player" {
                self.set_page("player");
            }
            self.set_playlist_visible(undo.pane);
        }

        // The first of them in the playlist, where the list goes to show
        // them coming back
        let first = undo
            .runs
            .iter()
            .filter_map(|run| run.tracks.first())
            .min_by_key(|track| queue.position(track));
        if let Some(first) = first {
            self.reveal_queue_track(first);
        }
    }
}

// What the Undo of the toast about removed tracks puts back: the runs of
// tracks removed while it was up, in the order they were removed, and how
// the window was before the last of them, for a queue that removing them
// emptied
#[derive(Default)]
pub(super) struct QueueUndo {
    toast: glib::WeakRef<adw::Toast>,
    // The generation of the queue they were removed from
    generation: u64,
    runs: Vec<RemovedRun>,
    page: String,
    pane: bool,
    // The track that was playing, and where
    resume: Option<(Track, u64)>,
    // The order the tracks had before they were shuffled, which Remove All
    // drops
    unshuffled: Option<Vec<gio::File>>,
}

// The tracks of a list of results that can play, leaving out those whose
// files are missing. By position: the filter model's item type is
// GObject, which `iter::<Track>()` would refuse, though its items are
// tracks
fn playable(results: &gio::ListModel) -> impl Iterator<Item = Track> + '_ {
    (0..results.n_items())
        .filter_map(|pos| results.item(pos).and_downcast::<Track>())
        .filter(|track| !track.missing())
}
