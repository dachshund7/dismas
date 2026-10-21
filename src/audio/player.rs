// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::RefCell,
    fmt::{self, Display, Formatter},
    rc::Rc,
};

use async_channel::{Receiver, Sender};
use glib::clone;
use gtk::glib;
use log::{debug, error};

use crate::{
    application::ApplicationAction,
    audio::{
        Controller, GstBackend, InhibitController, MprisController, PlayerState, Queue, SleepTimer,
        Track, WaveformGenerator,
    },
};

#[derive(Clone, Debug)]
pub enum PlaybackAction {
    Play,
    Pause,
    Stop,
    SkipPrevious,
    SkipNext,

    UpdatePosition(u64, bool),
    VolumeChanged(f64),
    Repeat(RepeatMode),
    Seek(i64),
    SeekTo(u64),
    PlayNext,
    SleepTimerExpired,

    Raise,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "DismasRepeatMode")]
pub enum RepeatMode {
    #[default]
    Consecutive,
    RepeatAll,
    RepeatOne,
}

impl Display for RepeatMode {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            RepeatMode::Consecutive => write!(f, "consecutive"),
            RepeatMode::RepeatAll => write!(f, "repeat-all"),
            RepeatMode::RepeatOne => write!(f, "repeat-one"),
        }
    }
}

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "DismasReplayGainMode")]
pub enum ReplayGainMode {
    #[enum_value(name = "album")]
    Album,
    #[enum_value(name = "track")]
    Track,
    #[enum_value(name = "off")]
    #[default]
    Off,
}

impl From<i32> for ReplayGainMode {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Album,
            1 => Self::Track,
            2 => Self::Off,
            _ => panic!("invalid ReplayGainMode enum key"),
        }
    }
}

impl From<ReplayGainMode> for i32 {
    fn from(value: ReplayGainMode) -> Self {
        match value {
            ReplayGainMode::Album => 0,
            ReplayGainMode::Track => 1,
            ReplayGainMode::Off => 2,
        }
    }
}

#[derive(Debug)]
pub enum SeekDirection {
    Forward,
    Backwards,
}

pub struct AudioPlayer {
    app_sender: Sender<ApplicationAction>,
    receiver: RefCell<Option<Receiver<PlaybackAction>>>,
    backend: GstBackend,
    controllers: Vec<Box<dyn Controller>>,
    queue: Queue,
    state: PlayerState,
    sleep_timer: SleepTimer,
    waveform_generator: WaveformGenerator,
}

impl fmt::Debug for AudioPlayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioPlayer").finish()
    }
}

impl AudioPlayer {
    pub fn new(app_sender: Sender<ApplicationAction>) -> Rc<Self> {
        let (sender, r) = async_channel::unbounded();
        let receiver = RefCell::new(Some(r));

        let mut controllers: Vec<Box<dyn Controller>> = Vec::new();

        let mpris_controller = MprisController::new(sender.clone());
        controllers.push(Box::new(mpris_controller));

        let inhibit_controller = InhibitController::new();
        controllers.push(Box::new(inhibit_controller));

        let waveform_generator = WaveformGenerator::new();
        controllers.push(Box::new(waveform_generator.clone()));

        let sleep_timer = SleepTimer::new(sender.clone());

        let backend = GstBackend::new(sender);

        let queue = Queue::default();
        let state = PlayerState::default();

        let res = Rc::new(Self {
            app_sender,
            receiver,
            backend,
            controllers,
            queue,
            state,
            sleep_timer,
            waveform_generator,
        });

        res.clone().setup_channel();

        res
    }

    fn setup_channel(self: Rc<Self>) {
        let receiver = self.receiver.borrow_mut().take().unwrap();

        glib::MainContext::default().spawn_local(clone!(
            #[strong(rename_to = this)]
            self,
            async move {
                use futures::prelude::*;

                let mut receiver = std::pin::pin!(receiver);
                while let Some(action) = receiver.next().await {
                    this.process_action(action);
                }
            }
        ));
    }

    fn process_action(&self, action: PlaybackAction) {
        match action {
            PlaybackAction::Play => self.set_playback_state(PlaybackState::Playing),
            PlaybackAction::Pause => self.set_playback_state(PlaybackState::Paused),
            PlaybackAction::Stop => self.set_playback_state(PlaybackState::Stopped),
            PlaybackAction::SkipPrevious => self.skip_previous(),
            PlaybackAction::SkipNext => self.skip_next(),
            PlaybackAction::UpdatePosition(pos, notify) => self.update_position(pos, notify),
            PlaybackAction::VolumeChanged(vol) => self.update_volume(vol),
            PlaybackAction::PlayNext => self.play_next(),
            PlaybackAction::SleepTimerExpired => self.sleep_timer_expired(),
            PlaybackAction::Raise => self.present(),
            PlaybackAction::Repeat(mode) => self.update_repeat_mode(mode),
            PlaybackAction::Seek(offset) => self.seek_offset(offset),
            PlaybackAction::SeekTo(position) => self.seek_position_abs(position),
        }
    }

    fn set_playback_state(&self, state: PlaybackState) {
        if let Some(current_track) = self.state.track() {
            debug!("Current track: {}", current_track.uri());

            self.state.set_playback_state(state);

            for c in &self.controllers {
                c.set_playback_state(state);
            }

            match state {
                PlaybackState::Playing => self.backend.play(),
                PlaybackState::Paused => self.backend.pause(),
                PlaybackState::Stopped => self.backend.stop(),
            }
        } else {
            debug!("Getting the next track");
            if let Some(next_track) = self.queue.next_track() {
                debug!("Next track: {}", next_track.uri());

                for c in &self.controllers {
                    c.set_track(&next_track);
                }

                next_track.set_playing(true);

                self.backend.set_track_uri(Some(&next_track.uri()));
                self.state.set_current_track(Some(next_track));
                self.state.set_playback_state(state);

                for c in &self.controllers {
                    c.set_playback_state(state);
                }

                match state {
                    PlaybackState::Playing => self.backend.play(),
                    PlaybackState::Paused => self.backend.pause(),
                    PlaybackState::Stopped => self.backend.stop(),
                }
            } else {
                debug!("No tracks left");
                self.backend.set_track_uri(None);
                self.state.set_current_track(None);
                self.state.set_playback_state(PlaybackState::Stopped);

                for c in &self.controllers {
                    c.set_playback_state(PlaybackState::Stopped);
                }
            }
        }
    }

    // Called at the end of a track
    fn play_next(&self) {
        if self.sleep_timer.finishing_track() {
            // Go on to the next track without playing it, so playing
            // again starts there
            self.sleep_timer.cancel();
            self.set_playback_state(PlaybackState::Paused);
        }

        self.skip_next();
    }

    // Pausing keeps the position, to go on from there later
    fn sleep_timer_expired(&self) {
        if self.state.playing() && self.sleep_timer.finish_track() {
            self.sleep_timer.wait_for_track_end();
        } else {
            self.sleep_timer.cancel();
            self.pause();
        }
    }

    pub fn toggle_play(&self) {
        if self.queue.is_empty() {
            return;
        }

        if self.state.playing() {
            self.set_playback_state(PlaybackState::Paused);
        } else {
            self.set_playback_state(PlaybackState::Playing);
        }
    }

    pub fn play(&self) {
        if !self.state.playing() {
            self.set_playback_state(PlaybackState::Playing);
        }
    }

    pub fn pause(&self) {
        if self.state.playing() {
            self.set_playback_state(PlaybackState::Paused);
        }
    }

    pub fn stop(&self) {
        self.set_playback_state(PlaybackState::Stopped);
    }

    pub fn skip_previous(&self) {
        if self.queue.is_empty() {
            return;
        }

        if let Some(current_track) = self.state.track() {
            // We only skip to the previous track if we are
            // within a seek backward step, otherwise we just
            // restart the track
            if self.state.position() >= 10 {
                self.backend.seek_start();
                return;
            }

            if self.queue.is_first_track() {
                return;
            }

            debug!("Marking '{}' as not playing", current_track.uri());
            current_track.set_playing(false);
        }

        if let Some(prev_track) = self.queue.previous_track() {
            debug!("Playing previous: {}", prev_track.uri());

            let was_playing = self.state.playing();
            if was_playing {
                self.set_playback_state(PlaybackState::Paused);
            }

            for c in &self.controllers {
                c.set_track(&prev_track);
            }

            self.backend.set_track_uri(Some(&prev_track.uri()));
            self.backend.seek_start();

            debug!("Marking '{}' as playing", prev_track.uri());
            prev_track.set_playing(true);

            self.state.set_current_track(Some(prev_track));

            if was_playing {
                self.set_playback_state(PlaybackState::Playing);
            }
        }
    }

    pub fn skip_next(&self) {
        self.play_next_track(Queue::next_track);
    }

    // Goes on to the track that `next_track` picks in the queue, or stops
    // at the first track if there is none
    fn play_next_track(&self, next_track: impl FnOnce(&Queue) -> Option<Track>) {
        if self.queue.is_empty() {
            return;
        }

        if let Some(current_track) = self.state.track() {
            current_track.set_playing(false);
        }

        if let Some(next_track) = next_track(&self.queue) {
            debug!("Playing next (skip-next): {}", next_track.uri());

            let was_playing = self.state.playing();
            if was_playing {
                self.set_playback_state(PlaybackState::Paused);
            }

            for c in &self.controllers {
                c.set_track(&next_track);
            }

            self.backend.set_track_uri(Some(&next_track.uri()));
            self.backend.seek_start();

            next_track.set_playing(true);

            self.state.set_current_track(Some(next_track));

            if was_playing {
                self.set_playback_state(PlaybackState::Playing);
            }
        } else {
            self.skip_to(0);
            self.set_playback_state(PlaybackState::Stopped);
        }
    }

    pub fn skip_to(&self, pos: u32) {
        if pos >= self.queue.n_tracks() {
            return;
        }

        if Some(pos) == self.queue.current_track_index() {
            return;
        }

        if let Some(current_track) = self.state.track() {
            current_track.set_playing(false);
        }

        if let Some(track) = self.queue.skip_track(pos) {
            debug!("Playing next (skip-to): {}", track.uri());
            let was_playing = self.state.playing();
            if was_playing {
                self.set_playback_state(PlaybackState::Paused);
            }

            for c in &self.controllers {
                c.set_track(&track);
            }

            self.backend.set_track_uri(Some(&track.uri()));
            self.backend.seek_start();

            track.set_playing(true);

            self.state.set_current_track(Some(track));

            if was_playing {
                self.set_playback_state(PlaybackState::Playing);
            }
        } else {
            self.backend.set_track_uri(None);
            self.state.set_current_track(None);
            self.set_playback_state(PlaybackState::Stopped);
        }
    }

    fn seek(&self, offset: u64, direction: SeekDirection) {
        self.backend.seek(
            self.state.position(),
            self.state.duration(),
            offset,
            direction,
        );
    }

    pub fn seek_start(&self) {
        let position = self.state.position() + 1;
        self.backend.seek(
            position,
            self.state.duration(),
            position,
            SeekDirection::Backwards,
        );
    }

    pub fn seek_backwards(&self) {
        self.seek(10, SeekDirection::Backwards);
    }

    pub fn seek_forward(&self) {
        self.seek(10, SeekDirection::Forward);
    }

    pub fn seek_offset(&self, offset: i64) {
        let direction = if offset < 0 {
            SeekDirection::Backwards
        } else {
            SeekDirection::Forward
        };
        self.seek(offset.unsigned_abs(), direction);
    }

    pub fn seek_position_rel(&self, position: f64) {
        let duration = self.state.duration() as f64;
        let pos = (duration * position).clamp(0.0, duration);
        self.backend.seek_position(pos as u64);
    }

    pub fn seek_position_abs(&self, position: u64) {
        let pos = u64::min(position, self.state.duration());
        self.backend.seek_position(pos);
    }

    // Shows the track paused at the position of the last session; the
    // backend only gets it ready when it plays
    pub fn resume_at(&self, position: u64) {
        let duration = self.state.duration();
        let position = if duration > 0 && position >= duration {
            0
        } else {
            position
        };
        // Shown and saved right away
        self.update_position(position, false);
        if position > 0 {
            self.backend.resume_at(position);
        }
        self.state.set_playback_state(PlaybackState::Paused);
        for c in &self.controllers {
            c.set_playback_state(PlaybackState::Paused);
        }
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    pub fn sleep_timer(&self) -> &SleepTimer {
        &self.sleep_timer
    }

    pub fn waveform_generator(&self) -> &WaveformGenerator {
        &self.waveform_generator
    }

    pub fn set_current_track(&self, track: Option<Track>) {
        self.state.set_current_track(track);
    }

    // Shows the new tags of the current track, after they were read again
    pub fn current_track_changed(&self) {
        let Some(track) = self.state.track() else {
            return;
        };
        self.state.track_changed();
        for c in &self.controllers {
            c.update_track(&track);
        }
    }

    fn update_position(&self, position: u64, notify: bool) {
        self.state.set_position(position);

        for c in &self.controllers {
            c.set_position(position, notify);
        }
    }

    fn update_volume(&self, volume: f64) {
        debug!("Updating volume to: {volume}");
        self.state.set_volume(volume);
    }

    pub fn set_volume(&self, volume: f64) {
        self.backend.set_volume(volume);
    }

    pub fn toggle_repeat_mode(&self) {
        let cur_mode = self.queue.repeat_mode();
        let new_mode = match cur_mode {
            RepeatMode::Consecutive => RepeatMode::RepeatAll,
            RepeatMode::RepeatAll => RepeatMode::RepeatOne,
            RepeatMode::RepeatOne => RepeatMode::Consecutive,
        };
        self.queue.set_repeat_mode(new_mode);

        for c in &self.controllers {
            c.set_repeat_mode(new_mode);
        }
    }

    fn update_repeat_mode(&self, repeat: RepeatMode) {
        if repeat != self.queue.repeat_mode() {
            self.queue.set_repeat_mode(repeat);

            for c in &self.controllers {
                c.set_repeat_mode(repeat);
            }
        }
    }

    fn present(&self) {
        if let Err(e) = self.app_sender.try_send(ApplicationAction::Present) {
            error!("Unable to send Present: {e}");
        }
    }

    pub fn clear_queue(&self) {
        self.stop();
        self.state.set_current_track(None);
        self.queue.clear();
    }

    // Moves tracks together before another one, or to the end of the
    // queue, keeping the order they had
    pub fn move_tracks(&self, tracks: &[Track], before: Option<&Track>) {
        self.queue.move_tracks(tracks, before);
    }

    pub fn remove_track(&self, track: &Track) {
        // Repeating the track would keep playing it after it's gone
        if track.playing() {
            self.play_next_track(Queue::following_track);
        }

        self.queue.remove_track(track);

        if self.queue.is_empty() {
            self.state.set_current_track(None);
        }
    }

    pub fn set_replaygain(&self, replaygain: ReplayGainMode) {
        self.backend.set_replaygain(replaygain);
    }

    pub fn replaygain_available(&self) -> bool {
        self.backend.replaygain_available()
    }
}
