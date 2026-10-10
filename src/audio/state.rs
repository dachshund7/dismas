// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::audio::{PlaybackState, Track};

mod imp {
    use glib::{
        ParamSpec, ParamSpecBoolean, ParamSpecDouble, ParamSpecObject, ParamSpecString,
        ParamSpecUInt64,
    };
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Debug)]
    pub struct PlayerState {
        pub playback_state: Cell<PlaybackState>,
        pub position: Cell<u64>,
        pub current_track: RefCell<Option<Track>>,
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

    impl ObjectImpl for PlayerState {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecBoolean::builder("playing").read_only().build(),
                    ParamSpecUInt64::builder("position").read_only().build(),
                    ParamSpecObject::builder::<Track>("track")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("title").read_only().build(),
                    ParamSpecString::builder("artist").read_only().build(),
                    ParamSpecString::builder("album").read_only().build(),
                    ParamSpecUInt64::builder("duration").read_only().build(),
                    ParamSpecObject::builder::<gdk::Texture>("cover")
                        .read_only()
                        .build(),
                    ParamSpecDouble::builder("volume")
                        .minimum(0.0)
                        .maximum(1.0)
                        .default_value(1.0)
                        .read_only()
                        .build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "playing" => obj.playing().to_value(),
                "position" => obj.position().to_value(),
                "track" => self.current_track.borrow().to_value(),
                "volume" => obj.volume().to_value(),

                // These are proxies for Track properties
                "title" => obj.title().to_value(),
                "artist" => obj.artist().to_value(),
                "album" => obj.album().to_value(),
                "duration" => obj.duration().to_value(),
                "cover" => obj.cover().to_value(),
                _ => unimplemented!(),
            }
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
    pub fn title(&self) -> Option<String> {
        if let Some(track) = &*self.imp().current_track.borrow() {
            return Some(track.title());
        }

        None
    }

    pub fn artist(&self) -> Option<String> {
        if let Some(track) = &*self.imp().current_track.borrow() {
            return Some(track.artist());
        }

        None
    }

    pub fn album(&self) -> Option<String> {
        if let Some(track) = &*self.imp().current_track.borrow() {
            return Some(track.album());
        }

        None
    }

    pub fn duration(&self) -> u64 {
        if let Some(track) = &*self.imp().current_track.borrow() {
            return track.duration();
        }

        0
    }

    pub fn cover(&self) -> Option<gdk::Texture> {
        if let Some(track) = &*self.imp().current_track.borrow() {
            return track.cover_texture();
        }

        None
    }

    pub fn playing(&self) -> bool {
        let playback_state = self.imp().playback_state.get();
        matches!(playback_state, PlaybackState::Playing)
    }

    pub fn set_playback_state(&self, playback_state: &PlaybackState) -> bool {
        let old_state = self.imp().playback_state.replace(*playback_state);
        if old_state != *playback_state {
            self.notify("playing");
            return true;
        }

        false
    }

    pub fn current_track(&self) -> Option<Track> {
        (*self.imp().current_track.borrow()).as_ref().cloned()
    }

    pub fn set_current_track(&self, track: Option<Track>) {
        self.imp().current_track.replace(track);
        self.imp().position.replace(0);
        self.notify("track");
        self.notify("title");
        self.notify("artist");
        self.notify("album");
        self.notify("duration");
        self.notify("cover");
        self.notify("position");
    }

    pub fn position(&self) -> u64 {
        self.imp().position.get()
    }

    pub fn set_position(&self, position: u64) {
        self.imp().position.replace(position);
        self.notify("position");
    }

    pub fn volume(&self) -> f64 {
        self.imp().volume.get()
    }

    pub fn set_volume(&self, volume: f64) {
        let old_volume = self.imp().volume.replace(volume);
        // We only care about two digits of precision, to avoid
        // notification cycles when we update the volume with a
        // similar value coming from the volume control
        let old_rounded = format!("{:.2}", old_volume);
        let new_rounded = format!("{:.2}", volume);
        if old_rounded != new_rounded {
            self.notify("volume");
        }
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        glib::Object::new()
    }
}
