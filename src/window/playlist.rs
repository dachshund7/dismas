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
                list_item.set_child(Some(&row));

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
                    move |row, x, y| win.show_track_menu(row, x, y, TrackMenu::Queue)
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

                row.bind_list_item(list_item);
            }
        ));
        imp.playlist_view
            .queue_view()
            .set_factory(Some(&factory.upcast::<gtk::ListItemFactory>()));
        imp.playlist_view
            .queue_view()
            .set_header_factory(Some(&work_header_factory(clone!(
                #[weak(rename_to = win)]
                self,
                move |start, end, ctrl| win.toggle_queue_work(start, end, ctrl)
            ))));

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
    }

    pub(super) fn reset_queue(&self) {
        // The sidebar shows the tracks of the playlist replacing these
        if self.imp().pending_playlist.borrow().is_none() {
            self.set_playlist_visible(false);
        }
        self.set_queue_shuffled(false);
        self.set_queue_selection(false);
        self.update_style(None);
    }

    pub(super) fn clear_queue(&self) {
        if let Some(p) = self.player() {
            p.clear_queue();

            // So that the queue isn't restored at the next startup
            self.store_queue();
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

    pub(super) fn scroll_queue_to_track(&self) {
        let queue_view = self.imp().playlist_view.queue_view();
        if let Some(player) = self.player()
            && let Some(current_idx) = player.queue().current_track_index()
        {
            debug!("Scrolling queue to {current_idx}");
            queue_view
                .upcast_ref::<gtk::Widget>()
                .activate_action("list.scroll-to-item", Some(&current_idx.to_variant()))
                .expect("Failed to activate action");
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
