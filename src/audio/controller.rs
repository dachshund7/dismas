// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::audio::{PlaybackState, RepeatMode, Track};

pub trait Controller {
    fn set_playback_state(&self, state: PlaybackState);

    fn set_track(&self, track: &Track);
    // The tags of the current track were read again
    fn update_track(&self, _track: &Track) {}
    fn set_position(&self, position: u64, notify: bool);
    fn set_repeat_mode(&self, repeat: RepeatMode);
}
