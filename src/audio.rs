// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

// Audio playback is slightly more complicated than simple UI handling code.
//
// We want to bind the state of the UI to the state of the player through
// well established patterns of property bindings and signal emissions, but
// the player can be updated from different threads and by different contexts
// which has an effect on the types that are in play. Of course, we can work
// around it by telling GStreamer that we want to receive signals in the
// default main context, but it's always largely iffy to do so as it requires
// working against the type system through the use of wrappers like Rc and
// Fragile.

// To avoid this mess, the best practice is to use message passing.
//
// In this particular case, the design of the audio playback interface is
// split into the following components:
//
// AudioPlayer: the main object managing the audio playback
// ├── PlayerState: the state tracker GObject used by the UI
// ├── Queue: the GListModel of the tracks to play
// ├── GstBackend: a GstPlayer wrapper
// ╰── controllers: external bits of code that interact with the state
//     ├── MprisController: an MPRIS wrapper
//     ╰── TrayController: a StatusNotifierItem (tray icon)
//
// The AudioPlayer object creates a glib::Sender/Receiver channel pair, and
// passes the sender to the controllers; whenever the controllers update their
// state, they will use the glib::Sender to notify the AudioPlayer, which will
// update the PlayerState.
//
// The GstBackend uses a similar sender/receiver pair to communicate with the
// AudioPlayer whenever the GStreamer state changes.
//
// The UI side connects to the PlayerState object for state tracking; all
// changes to the state object happen in the main context by design.
//
// Playback actions are proxied to the AudioPlayer object from the controllers.

mod controller;
pub use controller::Controller;

mod cover_cache;
pub use cover_cache::CoverCache;

mod inhibit_controller;
mod mpris_controller;
pub use inhibit_controller::InhibitController;
pub use mpris_controller::MprisController;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod tray_controller;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub use tray_controller::TrayController;

mod gst_backend;
pub use gst_backend::{GstBackend, TrackSource};

mod player;
mod queue;
mod shuffle;
mod sleep_timer;
mod state;
mod track;
mod waveform_generator;

pub use player::{
    AudioPlayer, PlaybackAction, PlaybackState, RepeatMode, ReplayGainMode, SeekDirection,
    VOLUME_STEP,
};
pub use queue::Queue;
pub use sleep_timer::SleepTimer;
pub use state::PlayerState;
pub use track::{
    ExtraComments, FileDetails, Gains, MusicBrainzIds, Track, TrackCredits, load_tracks,
    normalize_search_text, plain_commas, read_tagged_file, search_key, tag_search_text,
    title_from_file_name, track_title,
};
pub use waveform_generator::WaveformGenerator;

// How long the files of the caches of covers and waveforms are kept when
// they aren't used
const CACHE_AGE: std::time::Duration = std::time::Duration::from_secs(90 * 24 * 60 * 60);

// Removes the cached covers and waveforms that weren't used for a while;
// this reads folders, so it must not be called on the main thread
pub fn remove_old_cache_files() {
    crate::utils::remove_old_files(&cover_cache::cache_dir(), CACHE_AGE);
    crate::utils::remove_old_files(&waveform_generator::cache_dir(), CACHE_AGE);
}
