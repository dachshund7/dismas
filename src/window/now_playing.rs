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

                // The duration is 0 until it's known, and dividing by it
                // would make the position NaN, which survives clamping
                let position = if duration > 0 {
                    elapsed as f64 / duration as f64
                } else {
                    0.0
                };
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
            self.scroll_queue_to_track();
            self.update_queue_time();
            self.update_title(state.track().as_ref());
            self.update_style(state.track().as_ref());
        }
        self.sync_track_favorite();
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
        self.update_backdrop();
    }

    // The cover shared by all the tracks of an album is too small for the
    // main view, so we replace it with the full resolution cover of the
    // current track, once it has been loaded in a separate thread
    fn load_full_cover(&self, track: &Track) {
        let uuid = track.cover_uuid();
        let loaded = self
            .imp()
            .full_cover
            .borrow()
            .as_ref()
            .filter(|(loaded, _)| Some(loaded) == uuid.as_ref())
            .map(|(_, cover)| cover.clone());
        if let Some(cover) = loaded {
            self.imp().track_cover.album_image().set_cover(Some(&cover));
            return;
        }

        let Some(path) = track.audio_file().path() else {
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
                    .set_cover(Some(&cover));
                if let Some(uuid) = uuid {
                    win.imp().full_cover.replace(Some((uuid, cover)));
                }
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

        // In high contrast, the colors of the cover would only take some of
        // the contrast away
        let palette = if !imp.settings.boolean("enable-recoloring")
            || adw::StyleManager::default().is_high_contrast()
        {
            None
        } else {
            track.and_then(Track::cover_palette)
        };

        // The accent color is only replaced when the cover has a color to
        // make it from; the stylesheet's rule for it would make the accent
        // color invalid without one, rather than keep the system's
        let accent = palette.as_ref().and(track).and_then(Track::cover_accent);

        if let Some(palette) = &palette {
            let mut css = String::from(":root {");
            for (i, color) in palette.iter().enumerate() {
                css.push_str(&format!("--background-color-{i}: {color};"));
            }
            for i in palette.len()..5 {
                css.push_str(&format!("--background-color-{i}: var(--window-bg-color);"));
            }
            if let Some(accent) = accent {
                css.push_str(&format!("--cover-accent: {accent};"));
            }
            css.push('}');
            imp.provider.load_from_string(&css);
        } else {
            imp.provider.load_from_string("");
        }

        set_css_class(&*imp.toast_overlay, "main-window", palette.is_some());
        set_css_class(&*imp.toast_overlay, "cover-accent", accent.is_some());

        // An open track information dialog uses the same colors, which are
        // gone without a cover
        if let Some(dialog) = adw::prelude::AdwApplicationWindowExt::visible_dialog(self)
            .and_downcast::<TrackInfoDialog>()
        {
            set_css_class(&dialog, "recolored", palette.is_some());
        }
    }

    pub(super) fn update_title(&self, track: Option<&Track>) {
        if let Some(track) = track {
            self.set_title(Some(&format!("{} - {}", track.artist(), track.title())));
        } else {
            self.set_title(Some("Dismas"));
        }
    }

    pub fn set_track_time(&self, elapsed: Option<u64>, remaining: Option<u64>) {
        if let Some(elapsed) = elapsed {
            self.imp()
                .elapsed_label
                .set_text(&utils::format_duration(elapsed));
        } else {
            self.imp().elapsed_label.set_text("0:00");
        }

        if let Some(remaining) = remaining {
            self.imp()
                .remaining_label
                .set_text(&utils::format_remaining_time(remaining));
        } else {
            self.imp().remaining_label.set_text("0:00");
        }
    }

    pub fn set_track_position(&self, position: f64) {
        self.imp().waveform_view.set_position(position);
        self.imp().seek_scale.set_value(position);
    }
}

fn set_css_class(widget: &impl IsA<gtk::Widget>, class: &str, set: bool) {
    if set {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}
