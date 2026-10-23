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
                    move |row, x, y| win.show_track_menu(row, x, y)
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

                row.set_reorderable(true);
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

        if let Some(player) = self.player() {
            let queue = player.queue();

            let filter = SearchFilter::new();
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

                    if let Some(player) = win.player() {
                        let queue = player.queue();

                        let mut real_pos = None;
                        for i in 0..queue.model().n_items() {
                            if let Some(item) = queue.model().item(i) {
                                let s = item.downcast::<Track>().unwrap();
                                if s == track {
                                    real_pos = Some(i);
                                    break;
                                }
                            }
                        }

                        if let Some(real_pos) = real_pos {
                            if win.queue_selection() {
                                win.toggle_queue_track(&track);
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
                }
            ));

            imp.queue_filtermodel
                .replace(Some(filter_model.upcast::<gio::ListModel>()));

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
                        if let Some(adjustment) = win.imp().playlist_view.queue_view().vadjustment()
                        {
                            adjustment.set_value(0.0);
                        }
                    }
                ));
        }

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
        if let Some(p) = self.player() {
            p.clear_queue();

            // So that the queue isn't restored at the next startup
            self.store_queue();
            playlists::drop_unshuffled(&self.selected_playlist());
            self.sync_queue_shuffled();
        }
    }

    pub(super) fn queue_is_empty(&self) -> bool {
        self.player().is_none_or(|p| p.queue().is_empty())
    }

    pub(super) fn update_queue_time(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            let n_tracks = queue.n_tracks();
            if let Some(current) = queue.current_track_index() {
                let mut remaining_time = 0;
                for pos in 0..n_tracks {
                    let track = queue.track_at(pos).unwrap();
                    if pos >= current {
                        remaining_time += track.duration();
                    }
                }

                let remaining_min = ((remaining_time - (remaining_time % 60)) / 60) as u32;
                let remaining_hrs = (remaining_min - (remaining_min % 60)) / 60;

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
    }

    // Where a track is in the list of the queue, which only has the tracks
    // matching the search, if there is one
    fn queue_view_position(&self, track: &Track) -> Option<u32> {
        let model = self.imp().playlist_view.queue_view().model()?;
        (0..model.n_items()).find(|&i| model.item(i).as_ref() == Some(track.upcast_ref()))
    }

    // Only the last track to go to is scrolled to, e.g. skipping several
    pub(super) fn scroll_queue_to_track(&self) {
        let imp = self.imp();
        if let Some(tick) = imp.queue_scroll_tick.take() {
            tick.remove();
        }
        let Some(track) = self.player().and_then(|p| p.queue().current()) else {
            return;
        };
        let view = imp.playlist_view.queue_view();
        let tick = crate::queue_row::reveal_track(&view, &track);
        imp.queue_scroll_tick.replace(Some(tick));
    }

    // Shows the playing track in the queue, in the middle of the list,
    // where it flashes, like going to the album of a track; a search that
    // hides it is cleared
    pub(super) fn show_playing_track(&self) {
        let Some(track) = self.player().and_then(|p| p.state().track()) else {
            return;
        };
        let view = self.imp().playlist_view.queue_view();
        let mut pos = self.queue_view_position(&track);
        if pos.is_none() && self.queue_search() {
            self.imp().playlist_view.queue_searchentry().set_text("");
            self.set_queue_search(false);
            pos = self.queue_view_position(&track);
        }
        let Some(pos) = pos else {
            return;
        };

        self.show_playlist_view();
        self.set_playlist_visible(true);
        crate::queue_row::flash_track(&view, pos, &track);
    }

    // Moves a track above or below another one, dropped on its row. In
    // selection mode, dragging a selected track moves all the selected
    // ones, together and in the order they had
    fn move_queue_track(&self, track: &Track, target: &Track, above: bool) {
        let Some(player) = self.player() else {
            return;
        };
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
        let Some(player) = self.player() else {
            return;
        };
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

    pub fn remove_track(&self, track: &Track) {
        if let Some(p) = self.player() {
            p.remove_track(track);
        }
        self.update_selected_count();
        self.update_queue_time();
    }
}
