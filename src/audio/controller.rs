// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use crate::audio::{PlaybackState, RepeatMode, Track};

pub(crate) trait Controller {
    fn set_playback_state(&self, state: PlaybackState);

    fn set_track(&self, track: &Track);
    // The tags of the current track were read again
    fn update_track(&self, _track: &Track) {}
    // Whether the current track is a favorite, or what was heard of it,
    // changed
    fn update_stats(&self, _track: &Track) {}
    // No track is current anymore
    fn clear_track(&self) {}
    fn set_position(&self, position: Duration, notify: bool);
    fn set_repeat_mode(&self, repeat: RepeatMode);
    // Whether a track comes after the current one
    fn set_can_go_next(&self, _can_go_next: bool) {}
    // Whether Previous would do anything now
    fn set_can_go_previous(&self, _can_go_previous: bool) {}
    // Whether the selected playlist is shuffled, and whether that can be
    // changed now
    fn set_shuffle(&self, _shuffled: bool, _can_change: bool) {}
    // Whether the window is fullscreen, and whether it can be changed,
    // which takes a window that shows
    fn set_fullscreen(&self, _fullscreen: bool, _can_change: bool) {}
    // The volume as the volume slider shows it, not as GStreamer takes it
    fn set_volume(&self, _volume: f64) {}
}
