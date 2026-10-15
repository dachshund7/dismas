// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    pub(super) fn update_play_button(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            let play_button = self.imp().playback_control.play_button();
            if state.playing() {
                play_button.set_icon_name("media-playback-pause-symbolic");
            } else {
                play_button.set_icon_name("media-playback-start-symbolic");
            }
        }
    }

    pub(super) fn update_position_labels(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            if state.track().is_some() {
                let elapsed = state.position();
                let duration = state.duration();
                let remaining = duration.saturating_sub(elapsed);
                self.set_track_time(Some(elapsed), Some(remaining));

                let position = state.position() as f64 / state.duration() as f64;
                self.set_track_position(position);
            } else {
                self.set_track_time(None, None);
                self.set_track_position(0.0);
            }
        }
    }

    pub(super) fn update_track(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            self.scroll_playlist_to_track();
            self.update_playlist_time();
            self.update_title(state.track().as_ref());
            self.update_style(state.track().as_ref());
        }
    }

    pub(super) fn update_cover(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            let track_cover = self.imp().track_cover.get();
            if let Some(cover) = state.cover() {
                track_cover.album_image().set_cover(Some(&cover));
                track_cover.show_cover_image(true);

                if let Some(track) = state.track() {
                    self.load_full_cover(&track);
                }
            } else {
                track_cover.album_image().set_cover(None::<&gdk::Texture>);
                track_cover.show_cover_image(false);
            }
        }
    }

    // The cover shared by all the tracks of an album is too small for the
    // main view, so we replace it with the full resolution cover of the
    // current track, once it has been loaded in a separate thread
    fn load_full_cover(&self, track: &Track) {
        let Some(path) = track.file().path() else {
            return;
        };
        let uri = track.uri();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let cover = gio::spawn_blocking(move || CoverCache::load_full_cover(&path))
                    .await
                    .ok()
                    .flatten();
                let Some(cover) = cover else {
                    return;
                };

                // The track might have changed while we were loading
                let Some(player) = win.player() else {
                    return;
                };
                if player.state().track().map(|t| t.uri()) != Some(uri) {
                    return;
                }

                win.imp()
                    .track_cover
                    .get()
                    .album_image()
                    .set_cover(Some(&cover.texture()));
            }
        ));
    }

    pub(super) fn setup_provider(&self) {
        let imp = self.imp();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &imp.provider, 400);
        }
    }

    pub(super) fn update_style(&self, track: Option<&Track>) {
        let imp = self.imp();

        if !imp.settings.boolean("enable-recoloring") {
            imp.provider.load_from_string("");
            imp.toast_overlay.remove_css_class("main-window");
            return;
        }

        if let Some(track) = track
            && let Some(bg_colors) = track.cover_palette()
        {
            let mut css = String::new();

            css.push_str(":root {");

            let n_colors = bg_colors.len();
            for (i, color) in bg_colors.iter().enumerate().take(n_colors) {
                let s = format!("--background-color-{}: {};", i, color);
                css.push_str(&s);
            }

            for i in n_colors + 1 - 1..5 {
                css.push_str(&format!(
                    "--background-color-{}: var(--window-bg-color);",
                    i
                ));
            }

            css.push('}');

            imp.provider.load_from_string(&css);
            if !imp.toast_overlay.has_css_class("main-window") {
                imp.toast_overlay.add_css_class("main-window");
            }

            self.action_set_enabled("win.enable-recoloring", true);

            return;
        }

        imp.provider.load_from_string("");
        imp.toast_overlay.remove_css_class("main-window");
        self.action_set_enabled("win.enable-recoloring", false);
    }

    fn update_title(&self, track: Option<&Track>) {
        if let Some(track) = track {
            self.set_title(Some(&format!("{} - {}", track.artist(), track.title())));
        } else {
            self.set_title(Some("Amberol"));
        }
    }

    pub fn set_track_time(&self, elapsed: Option<u64>, remaining: Option<u64>) {
        if let Some(elapsed) = elapsed {
            self.imp()
                .elapsed_label
                .set_text(&utils::format_time(elapsed as i64));
        } else {
            self.imp().elapsed_label.set_text("0:00");
        }

        if let Some(remaining) = remaining {
            self.imp()
                .remaining_label
                .set_text(&utils::format_remaining_time(remaining as i64));
        } else {
            self.imp().remaining_label.set_text("0:00");
        }
    }

    pub fn set_track_position(&self, position: f64) {
        self.imp().waveform_view.set_position(position);
    }
}
