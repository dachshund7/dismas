// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

// Gapless playback goes around GstPlay, to the playbin it wraps: GstPlay
// only loads a track by stopping the pipeline first. When the track is
// about to finish, the next one is handed to playbin, which plays it right
// after, and the player follows once it starts. Without a next track, the
// track ends, and the player loads the next one itself.
//
// GstPlay changes the state of the pipeline on its own thread, which can
// wait for the streaming thread the track is handed over from while that
// one waits for it, so a track is only handed over while the pipeline
// doesn't change state. GstPlay also loads tracks on its thread, after
// these calls return, so none is handed over until it says each track is
// loaded. Stopping loads the track again, as playbin would otherwise keep
// the one that was handed over

use std::{
    cell::Cell,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use async_channel::Sender;
use glib::clone;
use gst::prelude::*;
use gtk::{gio, gio::prelude::*, glib};
use log::{debug, error, warn};

use crate::audio::{PlaybackAction, ReplayGainMode, SeekDirection};

// How long after loading a track the next one can be handed over
const HAND_OVER_AFTER_LOAD: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
struct Gapless {
    // The track after the one the player shows
    next_uri: Option<String>,
    // The track handed to playbin, until the player follows it or another
    // track is loaded; only one at a time
    handed: Option<String>,
    // Whether the handed track started playing
    started: bool,
    // The tracks GstPlay is yet to load; each says when it's loaded
    pending_loads: u32,
    // When the player last loaded a track
    loaded_at: Option<Instant>,
    // The track the pipeline plays
    current_uri: Option<String>,
    // Changes whenever the player loads a track itself, so it can tell
    // the tracks that started before
    generation: u64,
}

// The state lock of an element, held unless someone else holds it
struct StateLock(gst::Element);

impl StateLock {
    fn try_lock(element: &gst::Element) -> Option<Self> {
        // SAFETY: the lock is part of the element, which outlives the guard
        let locked = unsafe {
            let element_ptr = element.as_ptr();
            gst::glib::ffi::g_rec_mutex_trylock(&raw mut (*element_ptr).state_lock)
                != gst::glib::ffi::GFALSE
        };
        locked.then(|| Self(element.clone()))
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        // SAFETY: locked in try_lock, on this thread
        unsafe {
            let element_ptr = self.0.as_ptr();
            gst::glib::ffi::g_rec_mutex_unlock(&raw mut (*element_ptr).state_lock);
        }
    }
}

#[derive(Debug)]
pub struct GstBackend {
    sender: Sender<PlaybackAction>,
    gst_player: gst_play::Play,
    replaygain: Option<GstReplayGain>,
    // Where the track goes on from once it plays, after the last session
    resume_position: Cell<Option<u64>>,
    gapless: Arc<Mutex<Gapless>>,
}

#[derive(Debug)]
pub struct GstReplayGain {
    rg_filter_bin: gst::Element,
    rg_volume: gst::Element,
}

fn send_update_position(sender: &Sender<PlaybackAction>, clock: gst::ClockTime, notify: bool) {
    let pos = clock.seconds();
    if let Err(e) = sender.try_send(PlaybackAction::UpdatePosition(pos, notify)) {
        error!("Failed to send UpdatePosition({pos}): {e}");
    }
}

impl GstReplayGain {
    pub fn new() -> Result<GstReplayGain, Box<dyn std::error::Error>> {
        let rg_volume = gst::ElementFactory::make_with_name("rgvolume", Some("rg volume"))?;
        let rg_limiter = gst::ElementFactory::make_with_name("rglimiter", Some("rg limiter"))?;

        let filter_bin = gst::Bin::builder().name("filter bin").build();
        filter_bin.add(&rg_volume)?;
        filter_bin.add(&rg_limiter)?;
        rg_volume.link(&rg_limiter)?;

        let pad_src = rg_limiter.static_pad("src").unwrap();
        pad_src.set_active(true).unwrap();
        let ghost_src = gst::GhostPad::with_target(&pad_src)?;
        filter_bin.add_pad(&ghost_src)?;

        let pad_sink = rg_volume.static_pad("sink").unwrap();
        pad_sink.set_active(true).unwrap();
        let ghost_sink = gst::GhostPad::with_target(&pad_sink)?;
        filter_bin.add_pad(&ghost_sink)?;

        // rgvolume only forgets the gain of a track at the end of its
        // stream, and a gapless switch has none, so a track without gain
        // tags would keep the one before it. Each new track sends it an end
        // of stream of its own, which goes no further than rgvolume
        let seqnum = gst::Seqnum::next();
        ghost_sink.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            if let Some(gst::PadProbeData::Event(ref event)) = info.data
                && event.type_() == gst::EventType::StreamStart
            {
                pad_sink.send_event(gst::event::Eos::builder().seqnum(seqnum).build());
            }
            gst::PadProbeReturn::Ok
        });
        let rg_volume_src = rg_volume.static_pad("src").unwrap();
        rg_volume_src.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| match info.data {
                Some(gst::PadProbeData::Event(ref event))
                    if event.type_() == gst::EventType::Eos && event.seqnum() == seqnum =>
                {
                    gst::PadProbeReturn::Drop
                }
                _ => gst::PadProbeReturn::Ok,
            },
        );

        Ok(Self {
            rg_filter_bin: filter_bin.upcast(),
            rg_volume,
        })
    }

    pub fn set_mode(&self, playbin: gst::Element, replaygain: ReplayGainMode) {
        let identity = gst::ElementFactory::make_with_name("identity", None).unwrap();

        let (filter, album_mode) = match replaygain {
            ReplayGainMode::Album => (self.rg_filter_bin.as_ref(), true),
            ReplayGainMode::Track => (self.rg_filter_bin.as_ref(), false),
            ReplayGainMode::Off => (&identity, true),
        };

        self.rg_volume.set_property("album-mode", album_mode);
        playbin.set_property("audio-filter", filter);
    }
}

impl GstBackend {
    pub fn new(sender: Sender<PlaybackAction>) -> Self {
        let gst_player = gst_play::Play::default();

        gst_player.set_video_track_enabled(false);

        let mut config = gst_player.config();
        config.set_position_update_interval(250);
        gst_player.set_config(config).unwrap();

        let res = Self {
            sender,
            gst_player,
            replaygain: GstReplayGain::new().ok(),
            resume_position: Cell::default(),
            gapless: Arc::default(),
        };

        res.setup_signals();
        res.setup_gapless();

        res
    }

    fn gapless(&self) -> MutexGuard<'_, Gapless> {
        self.gapless.lock().unwrap()
    }

    fn setup_gapless(&self) {
        let pipeline = self.gst_player.pipeline();

        // From a streaming thread
        let gapless = self.gapless.clone();
        pipeline.connect("about-to-finish", false, move |args| {
            let playbin = args[0].get::<gst::Element>().unwrap();
            // Until the track is handed over, the pipeline can't change state
            let Some(_state_lock) = StateLock::try_lock(&playbin) else {
                debug!("Not handing over the next track while the pipeline changes state");
                return None;
            };
            let next_uri = {
                let mut gapless = gapless.lock().unwrap();
                if gapless.pending_loads > 0 || gapless.handed.is_some() {
                    return None;
                }
                // Loading a track right after one is handed over can leave
                // GstPlay waiting forever, and tracks are loaded one after
                // the other while skipping
                if gapless
                    .loaded_at
                    .is_some_and(|at| at.elapsed() < HAND_OVER_AFTER_LOAD)
                {
                    return None;
                }
                let next_uri = gapless.next_uri.clone()?;

                // A missing track would stop the pipeline before this one ends
                if !gio::File::for_uri(&next_uri).query_exists(gio::Cancellable::NONE) {
                    return None;
                }
                gapless.handed = Some(next_uri.clone());
                next_uri
            };

            debug!("Handing over the next track: {next_uri}");
            playbin.set_property("uri", &next_uri);
            None
        });

        let bus = pipeline.bus().unwrap();
        bus.enable_sync_message_emission();
        let gapless = self.gapless.clone();
        let sender = self.sender.clone();
        bus.connect_sync_message(Some("stream-start"), move |_, _| {
            let mut gapless = gapless.lock().unwrap();
            if gapless.started {
                return;
            }
            let Some(uri) = gapless.handed.clone() else {
                return;
            };
            gapless.started = true;
            let generation = gapless.generation;
            drop(gapless);

            if let Err(e) = sender.try_send(PlaybackAction::TrackAdvanced(generation, uri)) {
                error!("Failed to send TrackAdvanced: {e}");
            }
        });
    }

    fn setup_signals(&self) {
        let bus = self.gst_player.message_bus();
        bus.set_sync_handler(clone!(
            #[strong(rename_to = sender)]
            self.sender,
            #[strong(rename_to = gapless)]
            self.gapless,
            move |_bus, msg| {
                let Ok(play_msg) = gst_play::PlayMessage::parse(msg) else {
                    return gst::BusSyncReply::Drop;
                };

                match play_msg {
                    // The pipeline was stopped before, so no track handed
                    // over comes after this
                    gst_play::PlayMessage::UriLoaded(_) => {
                        let mut gapless = gapless.lock().unwrap();
                        gapless.handed = None;
                        gapless.started = false;
                        gapless.pending_loads = gapless.pending_loads.saturating_sub(1);
                    }
                    gst_play::PlayMessage::Error(message) => {
                        error!("GStreamer error: {}", message.error());
                    }
                    gst_play::PlayMessage::Warning(message) => {
                        warn!("GStreamer warning: {}", message.error());
                    }
                    gst_play::PlayMessage::EndOfStream(_) => {
                        if let Err(e) = sender.try_send(PlaybackAction::PlayNext) {
                            error!("Failed to send PlayNext: {e}");
                        }
                    }
                    gst_play::PlayMessage::PositionUpdated(message) => {
                        if let Some(position) = message.position() {
                            send_update_position(&sender, position, false);
                        }
                    }
                    gst_play::PlayMessage::SeekDone(message) => {
                        if let Some(position) = message.position() {
                            send_update_position(&sender, position, true);
                        }
                    }
                    gst_play::PlayMessage::VolumeChanged(message) => {
                        let volume = gst_audio::StreamVolume::convert_volume(
                            gst_audio::StreamVolumeFormat::Linear,
                            gst_audio::StreamVolumeFormat::Cubic,
                            message.volume(),
                        );
                        if let Err(e) = sender.try_send(PlaybackAction::VolumeChanged(volume)) {
                            error!("Failed to send VolumeChanged({volume}): {e}");
                        }
                    }
                    _ => {}
                }

                gst::BusSyncReply::Drop
            }
        ));
    }

    pub fn set_track_uri(&self, uri: Option<&str>) {
        self.resume_position.take();
        {
            let mut gapless = self.gapless();
            gapless.generation += 1;
            gapless.handed = None;
            gapless.started = false;
            gapless.pending_loads += 1;
            gapless.loaded_at = Some(Instant::now());
            gapless.current_uri = uri.map(str::to_owned);
        }
        self.gst_player.set_uri(uri);
    }

    // The track that playbin gets once this one is about to finish
    pub fn set_next_uri(&self, uri: Option<&str>) {
        self.gapless().next_uri = uri.map(str::to_owned);
    }

    pub fn generation(&self) -> u64 {
        self.gapless().generation
    }

    // The player follows the track that was handed over
    pub fn confirm_switch(&self, uri: &str) {
        let mut gapless = self.gapless();
        gapless.handed = None;
        gapless.started = false;
        gapless.current_uri = Some(uri.to_owned());
    }

    pub fn seek(&self, position: u64, duration: u64, offset: u64, direction: SeekDirection) {
        let offset = gst::ClockTime::from_seconds(offset);
        let position = gst::ClockTime::from_seconds(position);
        let duration = gst::ClockTime::from_seconds(duration);

        self.resume_position.take();
        let destination = match direction {
            SeekDirection::Backwards if position >= offset => position.checked_sub(offset),
            SeekDirection::Backwards if position < offset => Some(gst::ClockTime::from_seconds(0)),
            SeekDirection::Forward if !duration.is_zero() && position + offset <= duration => {
                position.checked_add(offset)
            }
            SeekDirection::Forward if !duration.is_zero() && position + offset > duration => {
                Some(duration)
            }
            _ => None,
        };

        if let Some(destination) = destination {
            self.gst_player.seek(destination);
        }
    }

    pub fn seek_position(&self, position: u64) {
        self.resume_position.take();
        self.gst_player.seek(gst::ClockTime::from_seconds(position));
    }

    pub fn seek_start(&self) {
        self.resume_position.take();
        self.gst_player.seek(gst::ClockTime::from_seconds(0));
    }

    // The track starts at the position once it plays, unless it's moved
    // or changed before. Getting it ready to play now, paused, could leave
    // PulseAudio waiting for it to start, and the track stuck at the
    // position, if it starts a few seconds later
    pub fn resume_at(&self, position: u64) {
        self.resume_position.set(Some(position));
    }

    pub fn play(&self) {
        self.gst_player.play();
        // GstPlay seeks once the track is ready to play
        if let Some(position) = self.resume_position.take() {
            self.gst_player.seek(gst::ClockTime::from_seconds(position));
        }
    }

    pub fn pause(&self) {
        self.gst_player.pause();
    }

    // Loading the track again stops it too, and replaces a track that was
    // handed over, which playbin would keep instead
    pub fn stop(&self) {
        let current_uri = self.gapless().current_uri.clone();
        if current_uri.is_some() {
            self.set_track_uri(current_uri.as_deref());
        } else {
            self.resume_position.take();
            self.gst_player.stop();
        }
    }

    pub fn set_volume(&self, volume: f64) {
        let linear_volume = gst_audio::StreamVolume::convert_volume(
            gst_audio::StreamVolumeFormat::Cubic,
            gst_audio::StreamVolumeFormat::Linear,
            volume,
        );
        debug!("Setting volume to: {linear_volume}");
        self.gst_player.set_volume(linear_volume);
    }

    pub fn set_replaygain(&self, replaygain: ReplayGainMode) {
        if let Some(ref r) = self.replaygain {
            r.set_mode(self.gst_player.pipeline(), replaygain);
        }
    }

    pub fn replaygain_available(&self) -> bool {
        self.replaygain.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_track(pad: &gst::Pad, stream_id: &str, track_gain: Option<f64>) {
        pad.push_event(gst::event::StreamStart::new(stream_id));
        let caps = gst_audio::AudioInfo::builder(gst_audio::AudioFormat::F32le, 44100, 1)
            .build()
            .unwrap()
            .to_caps()
            .unwrap();
        pad.push_event(gst::event::Caps::new(&caps));
        pad.push_event(gst::event::Segment::new(&gst::FormattedSegment::<
            gst::ClockTime,
        >::new()));
        if let Some(gain) = track_gain {
            let mut tags = gst::TagList::new();
            tags.get_mut()
                .unwrap()
                .add::<gst::tags::TrackGain>(&gain, gst::TagMergeMode::Append);
            pad.push_event(gst::event::Tag::new(tags));
        }
        pad.push(gst::Buffer::with_size(4 * 441).unwrap()).unwrap();
    }

    #[test]
    fn replaygain_forgets_the_gain_of_the_track_before() {
        gst::init().unwrap();
        // rgvolume comes with the good plugins
        let Ok(replaygain) = GstReplayGain::new() else {
            return;
        };

        let pipeline = gst::Pipeline::new();
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .unwrap();
        pipeline
            .add_many([&replaygain.rg_filter_bin, &sink])
            .unwrap();
        replaygain.rg_filter_bin.link(&sink).unwrap();
        let src = gst::Pad::builder(gst::PadDirection::Src).build();
        src.set_active(true).unwrap();
        src.link(&replaygain.rg_filter_bin.static_pad("sink").unwrap())
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();

        let gain = || replaygain.rg_volume.property::<f64>("result-gain");
        push_track(&src, "a", Some(-10.0));
        assert_eq!(gain(), -10.0);
        // A gapless switch, to a track without gain tags
        push_track(&src, "b", None);
        assert_eq!(gain(), 0.0);
        push_track(&src, "c", Some(-4.0));
        assert_eq!(gain(), -4.0);

        pipeline.set_state(gst::State::Null).unwrap();
    }
}
