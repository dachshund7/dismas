// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    sync::LazyLock,
    time::Duration,
};

use glib::subclass::Signal;
use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::{
    audio::{PlaybackState, Track},
    i18n::i18n_k,
};

mod imp {
    use super::*;

    #[derive(Debug, glib::Properties)]
    #[properties(wrapper_type = super::PlayerState)]
    pub(crate) struct PlayerState {
        #[property(name = "playing", get = Self::playing, type = bool)]
        pub(crate) playback_state: Cell<PlaybackState>,
        // Where the track is; the property is in whole seconds, as the
        // labels show it, see `position_time()`
        #[property(name = "position", get = Self::position, type = u64)]
        pub(crate) position: Cell<Duration>,
        // These are proxies for Track properties
        #[property(name = "track", get, nullable)]
        #[property(name = "title", get = Self::title, nullable, type = Option<String>)]
        #[property(name = "artist", get = Self::artist, nullable, type = Option<String>)]
        #[property(name = "album", get = Self::album, nullable, type = Option<String>)]
        #[property(name = "album-line", get = Self::album_line, nullable, type = Option<String>)]
        #[property(name = "credits", get = Self::credits, nullable, type = Option<String>)]
        #[property(name = "heading", get = Self::heading, nullable, type = Option<String>)]
        #[property(name = "movement", get = Self::movement, nullable, type = Option<String>)]
        #[property(name = "duration", get = Self::duration, type = u64)]
        #[property(name = "cover", get = Self::cover, nullable, type = Option<gdk::Texture>)]
        pub(crate) current_track: RefCell<Option<Track>>,
        #[property(get, minimum = 0.0, maximum = 1.0, default = 1.0)]
        pub(crate) volume: Cell<f64>,
        // How many times a track was set to play, the same one again
        // included, so that the lyrics can tell a new play of the track
        // they show from the one they showed
        #[property(get)]
        pub(crate) starts: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerState {
        const NAME: &'static str = "DismasPlayerState";
        type Type = super::PlayerState;

        fn new() -> Self {
            Self {
                playback_state: Cell::new(PlaybackState::Stopped),
                position: Cell::new(Duration::ZERO),
                current_track: RefCell::new(None),
                volume: Cell::new(1.0),
                starts: Cell::new(0),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for PlayerState {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                // A track couldn't be played, and was skipped if any track
                // after it could
                vec![
                    Signal::builder("track-failed")
                        .param_types([Track::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl PlayerState {
        fn position(&self) -> u64 {
            self.position.get().as_secs()
        }

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

        // The album, and the year the track was made, for the player
        fn album_line(&self) -> Option<String> {
            let track = self.current_track.borrow();
            let track = track.as_ref()?;
            let album = track.album();
            Some(match track.year() {
                Some(year) => i18n_k(
                    // Translators: `{album}` and `{year}` must be left
                    // unmodified; they are expanded to the album of the
                    // track playing and the year it was recorded or first
                    // released
                    "{album} · {year}",
                    &[("album", &album), ("year", &year)],
                ),
                None => album,
            })
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
    pub(crate) struct PlayerState(ObjectSubclass<imp::PlayerState>);
}

impl PlayerState {
    pub(crate) fn stopped(&self) -> bool {
        self.imp().playback_state.get() == PlaybackState::Stopped
    }

    pub(crate) fn set_playback_state(&self, playback_state: PlaybackState) -> bool {
        let old_state = self.imp().playback_state.replace(playback_state);
        if old_state != playback_state {
            self.notify_playing();
            return true;
        }

        false
    }

    pub(crate) fn set_current_track(&self, track: Option<Track>) {
        // The same track again, as when it repeats, only starts over: the
        // cover, the labels and the queue stay as they are
        if track.is_some() && self.track() == track {
            self.count_start();
            self.set_position(Duration::ZERO);
            return;
        }

        self.imp().current_track.replace(track);
        self.imp().position.replace(Duration::ZERO);
        // Once the track is in place, so that a view sees it's another one
        self.count_start();
        self.notify_track();
        self.notify_title();
        self.notify_artist();
        self.notify_album();
        self.notify_album_line();
        self.notify_credits();
        self.notify_heading();
        self.notify_movement();
        self.notify_duration();
        self.notify_cover();
        self.notify_position();
    }

    fn count_start(&self) {
        let starts = &self.imp().starts;
        starts.set(starts.get() + 1);
        self.notify_starts();
    }

    // Shows the new tags of the current track, after it was updated
    pub(crate) fn track_changed(&self) {
        self.notify_title();
        self.notify_artist();
        self.notify_album();
        self.notify_album_line();
        self.notify_credits();
        self.notify_heading();
        self.notify_movement();
        self.notify_duration();
        self.notify_cover();
    }

    pub(crate) fn emit_track_failed(&self, track: &Track) {
        self.emit_by_name::<()>("track-failed", &[track]);
    }

    pub(crate) fn connect_track_failed<F: Fn(&Self, &Track) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "track-failed",
            false,
            glib::closure_local!(move |state: &Self, track: &Track| f(state, track)),
        )
    }

    // Notifies at each position the backend reports, about four times a
    // second, even within the same second, as the lyrics follow the time
    // between the seconds
    pub(crate) fn set_position(&self, position: Duration) {
        self.imp().position.replace(position);
        self.notify_position();
    }

    // The position to the millisecond
    pub(crate) fn position_time(&self) -> Duration {
        self.imp().position.get()
    }

    // The length of the current track, zero if it's unknown; the `duration`
    // property is in whole seconds
    pub(crate) fn length(&self) -> Duration {
        self.track().map_or(Duration::ZERO, |track| track.length())
    }

    pub(crate) fn set_volume(&self, volume: f64) {
        let old_volume = self.imp().volume.replace(volume);
        // We only care about two digits of precision, to avoid
        // notification cycles when we update the volume with a
        // similar value coming from the volume control
        let hundredths = |volume: f64| (volume * 100.0).round();
        if hundredths(old_volume) != hundredths(volume) {
            self.notify_volume();
        }
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        glib::Object::new()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::audio::track::TrackData;

    fn record_notifies(state: &PlayerState) -> Rc<RefCell<Vec<String>>> {
        let notified = Rc::new(RefCell::new(Vec::new()));
        let recorded = notified.clone();
        state.connect_notify_local(None, move |_, pspec| {
            recorded.borrow_mut().push(pspec.name().to_string());
        });
        notified
    }

    #[test]
    fn same_track_only_starts_over() {
        let state = PlayerState::default();
        let track = Track::new(TrackData::default());
        state.set_current_track(Some(track.clone()));
        state.set_position(Duration::from_millis(42_500));

        let starts = state.starts();
        let notified = record_notifies(&state);
        state.set_current_track(Some(track));
        assert_eq!(state.position(), 0);
        assert_eq!(state.starts(), starts + 1);
        assert_eq!(*notified.borrow(), ["starts", "position"]);
    }

    #[test]
    fn another_track_is_a_change() {
        let state = PlayerState::default();
        state.set_current_track(Some(Track::new(TrackData::default())));

        let starts = state.starts();
        let notified = record_notifies(&state);
        state.set_current_track(Some(Track::new(TrackData::default())));
        assert_eq!(state.starts(), starts + 1);
        let notified = notified.borrow();
        assert!(notified.iter().any(|name| name == "track"));
        assert!(notified.iter().any(|name| name == "starts"));
        assert!(notified.iter().any(|name| name == "cover"));
    }
}
