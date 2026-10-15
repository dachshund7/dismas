// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::audio::{PlaybackState, Track};

mod imp {
    use super::*;

    #[derive(Debug, glib::Properties)]
    #[properties(wrapper_type = super::PlayerState)]
    pub struct PlayerState {
        #[property(name = "playing", get = Self::playing, type = bool)]
        pub playback_state: Cell<PlaybackState>,
        #[property(get)]
        pub position: Cell<u64>,
        // These are proxies for Track properties
        #[property(name = "track", get, nullable)]
        #[property(name = "title", get = Self::title, nullable, type = Option<String>)]
        #[property(name = "artist", get = Self::artist, nullable, type = Option<String>)]
        #[property(name = "album", get = Self::album, nullable, type = Option<String>)]
        #[property(name = "credits", get = Self::credits, nullable, type = Option<String>)]
        #[property(name = "heading", get = Self::heading, nullable, type = Option<String>)]
        #[property(name = "movement", get = Self::movement, nullable, type = Option<String>)]
        #[property(name = "duration", get = Self::duration, type = u64)]
        #[property(name = "cover", get = Self::cover, nullable, type = Option<gdk::Texture>)]
        pub current_track: RefCell<Option<Track>>,
        #[property(get, minimum = 0.0, maximum = 1.0, default = 1.0)]
        pub volume: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerState {
        const NAME: &'static str = "AmberolPlayerState";
        type Type = super::PlayerState;

        fn new() -> Self {
            Self {
                playback_state: Cell::new(PlaybackState::Stopped),
                position: Cell::new(0),
                current_track: RefCell::new(None),
                volume: Cell::new(1.0),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for PlayerState {}

    impl PlayerState {
        fn playing(&self) -> bool {
            matches!(self.playback_state.get(), PlaybackState::Playing)
        }

        fn title(&self) -> Option<String> {
            self.current_track.borrow().as_ref().map(Track::title)
        }

        fn artist(&self) -> Option<String> {
            self.current_track.borrow().as_ref().map(Track::artist)
        }

        fn album(&self) -> Option<String> {
            self.current_track.borrow().as_ref().map(Track::album)
        }

        fn credits(&self) -> Option<String> {
            self.current_track.borrow().as_ref().map(Track::credits)
        }

        fn heading(&self) -> Option<String> {
            self.current_track.borrow().as_ref().map(Track::heading)
        }

        fn movement(&self) -> Option<String> {
            self.current_track
                .borrow()
                .as_ref()
                .and_then(Track::movement)
        }

        fn duration(&self) -> u64 {
            self.current_track
                .borrow()
                .as_ref()
                .map_or(0, Track::duration)
        }

        fn cover(&self) -> Option<gdk::Texture> {
            self.current_track.borrow().as_ref().and_then(Track::cover)
        }
    }
}

// PlayerState is a GObject that we can use to bind to
// widgets and other objects; it contains the current
// state of the audio player: track metadata, playback
// position and duration, etc.
glib::wrapper! {
    pub struct PlayerState(ObjectSubclass<imp::PlayerState>);
}

impl PlayerState {
    pub fn set_playback_state(&self, playback_state: &PlaybackState) -> bool {
        let old_state = self.imp().playback_state.replace(*playback_state);
        if old_state != *playback_state {
            self.notify_playing();
            return true;
        }

        false
    }

    pub fn set_current_track(&self, track: Option<Track>) {
        self.imp().current_track.replace(track);
        self.imp().position.replace(0);
        self.notify_track();
        self.notify_title();
        self.notify_artist();
        self.notify_album();
        self.notify_credits();
        self.notify_heading();
        self.notify_movement();
        self.notify_duration();
        self.notify_cover();
        self.notify_position();
    }

    pub fn set_position(&self, position: u64) {
        self.imp().position.replace(position);
        self.notify_position();
    }

    pub fn set_volume(&self, volume: f64) {
        let old_volume = self.imp().volume.replace(volume);
        // We only care about two digits of precision, to avoid
        // notification cycles when we update the volume with a
        // similar value coming from the volume control
        let old_rounded = format!("{:.2}", old_volume);
        let new_rounded = format!("{:.2}", volume);
        if old_rounded != new_rounded {
            self.notify_volume();
        }
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        glib::Object::new()
    }
}
