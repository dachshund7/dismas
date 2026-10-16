// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    pub(super) fn setup_waveform(&self) {
        if let Some(player) = self.player() {
            let peaks = player.waveform_generator().peaks();
            self.imp().waveform_view.set_peaks(peaks);

            let notify_peaks_id = player.waveform_generator().connect_notify_local(
                Some("has-peaks"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |generator, _| {
                        let peaks = generator.peaks();
                        win.imp().waveform_view.set_peaks(peaks);
                    }
                ),
            );
            self.imp().notify_peaks_id.replace(Some(notify_peaks_id));
        }
    }

    fn unbind_waveform(&self) {
        if let Some(player) = self.player()
            && let Some(id) = self.imp().notify_peaks_id.take()
        {
            player.waveform_generator().disconnect(id);
        }
    }

    // Bind the PlayerState to the UI
    pub(super) fn bind_state(&self) {
        let imp = self.imp();
        if let Some(player) = self.player() {
            let state = player.state();

            // Use the PlayerState:playing property to control the play/pause button
            self.update_play_button();
            let notify_playing_id = state.connect_notify_local(
                Some("playing"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.set_playlist_selection(false);
                        win.update_play_button();
                    }
                ),
            );
            imp.notify_playing_id.replace(Some(notify_playing_id));

            // Update the position labels
            self.update_position_labels();
            let notify_position_id = state.connect_notify_local(
                Some("position"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_position_labels();
                    }
                ),
            );
            imp.notify_position_id.replace(Some(notify_position_id));

            // Update the UI
            self.update_track();
            let notify_track_id = state.connect_notify_local(
                Some("track"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_track();
                    }
                ),
            );
            imp.notify_track_id.replace(Some(notify_track_id));

            // Update the cover, if any is available
            self.update_cover();
            let notify_cover_id = state.connect_notify_local(
                Some("cover"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| {
                        win.update_cover();
                    }
                ),
            );
            imp.notify_cover_id.replace(Some(notify_cover_id));

            // Bind the track properties to the UI
            state
                .bind_property("heading", &imp.track_details.get().title_label(), "label")
                .sync_create()
                .build();
            state
                .bind_property("artist", &imp.track_details.get().artist_label(), "label")
                .sync_create()
                .build();
            state
                .bind_property("album", &imp.track_details.get().album_label(), "label")
                .sync_create()
                .build();
            state
                .bind_property(
                    "movement",
                    &imp.track_details.get().movement_label(),
                    "label",
                )
                .sync_create()
                .build();
            state
                .bind_property(
                    "movement",
                    &imp.track_details.get().movement_label(),
                    "visible",
                )
                .transform_to(|_, movement: Option<String>| Some(movement.is_some()))
                .sync_create()
                .build();
            state
                .bind_property("credits", &imp.track_details.get().credits_label(), "label")
                .sync_create()
                .build();
            // Most tracks have no credits, so we hide the line instead of
            // leaving an empty space
            state
                .bind_property(
                    "credits",
                    &imp.track_details.get().credits_label(),
                    "visible",
                )
                .transform_to(|_, credits: Option<String>| {
                    Some(credits.is_some_and(|c| !c.is_empty()))
                })
                .sync_create()
                .build();
            state
                .bind_property(
                    "volume",
                    &imp.playback_control.get().volume_control(),
                    "volume",
                )
                .sync_create()
                .build();
        }
    }

    fn unbind_state(&self) {
        if let Some(player) = self.player() {
            let state = player.state();

            if let Some(id) = self.imp().notify_playing_id.take() {
                state.disconnect(id);
            }
            if let Some(id) = self.imp().notify_position_id.take() {
                state.disconnect(id);
            }
            if let Some(id) = self.imp().notify_track_id.take() {
                state.disconnect(id);
            }
            if let Some(id) = self.imp().notify_cover_id.take() {
                state.disconnect(id);
            }
        }
    }

    // Bind the Queue to the UI
    pub(super) fn bind_queue(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();

            let notify_ntracks_id = queue.connect_notify_local(
                Some("n-tracks"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |queue, _| {
                        debug!("queue.n_tracks() = {}", queue.n_tracks());
                        if queue.is_empty() {
                            win.set_initial_state();
                            win.reset_queue();
                        } else {
                            win.update_library_for_queue();
                            win.action_set_enabled("queue.shuffle", queue.n_tracks() > 1);

                            win.action_set_enabled("win.play", true);
                            win.action_set_enabled("win.previous", true);
                            win.action_set_enabled("win.next", queue.n_tracks() > 1);
                        }

                        if queue.n_tracks() == 1
                            && let Some(p) = win.player()
                        {
                            p.skip_next();
                        }

                        win.update_playlist_time();
                    }
                ),
            );
            self.imp()
                .notify_ntracks_id
                .replace(Some(notify_ntracks_id));

            queue.connect_notify_local(
                Some("repeat-mode"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |queue, _| {
                        win.imp()
                            .playback_control
                            .set_repeat_mode(queue.repeat_mode());
                    }
                ),
            );

            let notify_current_id = queue.connect_notify_local(
                Some("current"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |queue, _| {
                        if queue.is_last_track() {
                            match queue.repeat_mode() {
                                RepeatMode::Consecutive => {
                                    win.action_set_enabled("win.next", false);
                                }
                                _ => {
                                    win.action_set_enabled("win.next", true);
                                }
                            }
                        } else {
                            win.action_set_enabled("win.next", true);
                        }
                    }
                ),
            );
            self.imp()
                .notify_current_id
                .replace(Some(notify_current_id));
        }
    }

    fn unbind_queue(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            if let Some(id) = self.imp().notify_ntracks_id.take() {
                queue.disconnect(id);
            }
            if let Some(id) = self.imp().notify_current_id.take() {
                queue.disconnect(id);
            }
        }
    }

    pub(super) fn connect_signals(&self) {
        self.imp().split_view.connect_notify_local(
            Some("collapsed"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |split_view, _| {
                    // The sidebar is pinned, so that it's only shown again
                    // when uncollapsing if it was showing before collapsing
                    let imp = win.imp();
                    if split_view.is_collapsed() {
                        imp.sidebar_visible_uncollapsed
                            .set(split_view.shows_sidebar());
                        split_view.set_show_sidebar(false);
                    } else if imp.sidebar_visible_uncollapsed.get() {
                        split_view.set_show_sidebar(true);
                    }
                    win.set_playlist_visible(split_view.shows_sidebar());
                    imp.playlist_view
                        .back_button()
                        .set_visible(split_view.is_collapsed());
                }
            ),
        );

        self.imp().split_view.connect_notify_local(
            Some("show-sidebar"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |split_view, _| {
                    win.set_playlist_visible(split_view.shows_sidebar());
                }
            ),
        );

        self.imp().waveform_view.connect_position_changed(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, position| {
                debug!("New position: {position}");
                if let Some(player) = win.player() {
                    let state = player.state();
                    if state.track().is_some() {
                        player.seek_position_rel(position);
                        player.play();
                    }
                }
            }
        ));

        self.imp()
            .playback_control
            .volume_control()
            .connect_volume_changed(clone!(
                #[weak(rename_to = win)]
                self,
                move |_, volume| {
                    debug!("Volume changed: {volume}");
                    if let Some(p) = win.player() {
                        p.set_volume(volume);
                    }
                }
            ));

        self.imp()
            .playlist_view
            .queue_select_all_button()
            .connect_clicked(clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.select_all_playlist_tracks()
            ));

        self.imp()
            .playlist_view
            .playlist_searchbar()
            .connect_notify_local(
                Some("search-mode-enabled"),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |searchbar, _| {
                        win.set_playlist_search(searchbar.is_search_mode());
                    }
                ),
            );

        self.imp().settings.connect_changed(
            Some("enable-recoloring"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |settings, _| {
                    debug!(
                        "GSettings:enable-recoloring: {}",
                        settings.boolean("enable-recoloring")
                    );
                    if let Some(player) = this.player() {
                        let state = player.state();
                        this.update_style(state.track().as_ref());
                    }
                }
            ),
        );
        let _dummy = self.imp().settings.boolean("enable-recoloring");

        self.connect_close_request(move |window| {
            debug!("Saving window state");
            let width = window.default_size().0;
            let height = window.default_size().1;

            let settings = utils::settings_manager();
            settings
                .set_int("window-width", width)
                .expect("Unable to store window-width");
            settings
                .set_int("window-height", height)
                .expect("Unable to stop window-height");

            window.unbind_queue();
            window.unbind_state();
            window.unbind_waveform();

            glib::Propagation::Proceed
        });

        self.imp()
            .playlist_view
            .playlist_searchbar()
            .set_key_capture_widget(Some(self.upcast_ref::<gtk::Widget>()));
    }

    // The initial state of the playback actions
    pub(super) fn set_initial_state(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            self.action_set_enabled("win.play", !queue.is_empty());
            self.action_set_enabled("win.previous", !queue.is_empty());
            self.action_set_enabled("win.next", !queue.is_last_track());

            self.action_set_enabled("queue.shuffle", queue.n_tracks() > 1);
            self.action_set_enabled("win.replaygain", player.replaygain_available());

            let replaygain = self.imp().settings.enum_("replay-gain").into();
            self.set_replaygain_mode(replaygain);

            // Manually set player state, because set_replaygain
            // only updates player state when the value changes.
            player.set_replaygain(replaygain);

            self.imp()
                .playback_control
                .set_repeat_mode(queue.repeat_mode());
            self.set_playlist_shuffled(queue.is_shuffled());

            self.update_library_for_queue();
        }
    }
}
