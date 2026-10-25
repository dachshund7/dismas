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
//
// The tracks of a CUE sheet are parts of one file. A track starts with a
// seek once its file is loaded: GstPlay drops the seeks made before it
// loads a file, so a track that doesn't start at the beginning of its file
// only plays once it's loaded. When the next track goes on from where the
// track ends in the same file, the pipeline plays on, and the player
// follows once the position is past the end. Otherwise, the audio after
// the end is cut before it reaches the sink, which ends the stream there.
// Only the next track of a file that plays to its end can be handed over,
// as playbin plays it from its start

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

use crate::audio::{Gains, PlaybackAction, ReplayGainMode, SeekDirection, Track};

// How long after loading a track the next one can be handed over
const HAND_OVER_AFTER_LOAD: Duration = Duration::from_millis(500);

// What plays for a track: its file, or the part of a file that a CUE sheet
// gives it
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSource {
    // The URI of the track, which tells apart the tracks of one file
    pub id: String,
    // The URI of the file that plays
    pub uri: String,
    pub start: gst::ClockTime,
    // Until the end of the file if `None`
    pub end: Option<gst::ClockTime>,
    // For a part of a file, as the stream has the gains of all of it
    pub gains: Option<Gains>,
}

impl TrackSource {
    pub fn of(track: &Track) -> Self {
        let (start, end) = track.segment();
        let clock_time = |d: Duration| gst::ClockTime::from_nseconds(d.as_nanos() as u64);
        Self {
            id: track.uri(),
            uri: track.audio_uri(),
            start: clock_time(start),
            end: end.map(clock_time),
            gains: track.gains(),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Gapless {
    // The track after the one the player shows
    next: Option<TrackSource>,
    // The track handed to playbin, until the player follows it or another
    // track is loaded; only one at a time
    handed: Option<TrackSource>,
    // Whether the handed track started playing
    started: bool,
    // The tracks GstPlay is yet to load; each says when it's loaded
    pending_loads: u32,
    // When the player last loaded a track
    loaded_at: Option<Instant>,
    // The track the player shows, which the pipeline plays
    current: Option<TrackSource>,
    // The track after `current` in the same file, which the pipeline went
    // on to, until the player follows it
    advanced: Option<TrackSource>,
    // Whether the audio after the end of the track was cut, as the next
    // track doesn't go on from there; until a seek or another track
    cut: bool,
    // Whether the last seek took the track to its start, which isn't
    // reported as a seek
    start_seek: bool,
    // Changes whenever the player loads a track itself, so it can tell
    // the tracks that started before
    generation: u64,
}

impl Gapless {
    // The track the pipeline plays
    fn playing(&self) -> Option<&TrackSource> {
        self.advanced.as_ref().or(self.current.as_ref())
    }

    // Whether the next track goes on from where `source` ends in its file
    fn next_follows(&self, source: &TrackSource) -> bool {
        source.end.is_some_and(|end| {
            self.next
                .as_ref()
                .is_some_and(|next| next.uri == source.uri && next.start == end)
        })
    }
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
    // Where the track starts once it plays, if it hasn't since it was
    // loaded, from the start of the track: GstPlay would only seek once
    // it's ready to play
    start_position: Cell<Option<u64>>,
    // Whether the track plays once it's loaded
    play_requested: Cell<bool>,
    gapless: Arc<Mutex<Gapless>>,
}

#[derive(Debug)]
pub struct GstReplayGain {
    rg_filter_bin: gst::Element,
    rg_volume: gst::Element,
}

// Reports the position from the start of the track playing, given the
// one in its file; past the end of the track, the next one goes on from
// there if it's the next part of the file
fn update_position(
    sender: &Sender<PlaybackAction>,
    gapless: &Mutex<Gapless>,
    position: gst::ClockTime,
    seek_done: bool,
) {
    let mut gapless = gapless.lock().unwrap();
    // From the file before the one being loaded
    if gapless.pending_loads > 0 {
        return;
    }
    let notify = seek_done && !std::mem::take(&mut gapless.start_seek);

    let ended = gapless
        .current
        .as_ref()
        .and_then(|current| current.end)
        .is_some_and(|end| position >= end);
    if ended && gapless.advanced.is_none() {
        let current = gapless.current.as_ref().unwrap();
        if gapless.next_follows(current) {
            let next = gapless.next.clone().unwrap();
            let action = PlaybackAction::TrackAdvanced(gapless.generation, next.id.clone());
            gapless.advanced = Some(next);
            if let Err(e) = sender.try_send(action) {
                error!("Failed to send TrackAdvanced: {e}");
            }
        }
    }

    let start = gapless
        .playing()
        .map_or(gst::ClockTime::ZERO, |playing| playing.start);
    drop(gapless);

    let pos = position.saturating_sub(start).seconds();
    if let Err(e) = sender.try_send(PlaybackAction::UpdatePosition(pos, notify)) {
        error!("Failed to send UpdatePosition({pos}): {e}");
    }
}

// A number of seconds as a clock time. Seek offsets come from other
// programs over MPRIS, and lengths from the headers of files, so a value
// too large for a clock time becomes the largest one rather than a panic
fn seconds(seconds: u64) -> gst::ClockTime {
    let nseconds = seconds
        .saturating_mul(gst::ClockTime::SECOND.nseconds())
        .min(gst::ClockTime::MAX.nseconds());
    gst::ClockTime::from_nseconds(nseconds)
}

// Where a seek of `offset` seconds from `position` goes, in seconds from
// the start of the track: back no further than the start, and forward no
// further than the end. Without a known length, there's no seeking forward
fn seek_destination(
    position: u64,
    duration: u64,
    offset: u64,
    direction: SeekDirection,
) -> Option<u64> {
    match direction {
        SeekDirection::Backwards => Some(position.saturating_sub(offset)),
        SeekDirection::Forward if duration > 0 => {
            Some(position.saturating_add(offset).min(duration))
        }
        SeekDirection::Forward => None,
    }
}

// Where a buffer is in its file
fn stream_time(pad: &gst::Pad, buffer: &gst::BufferRef) -> Option<gst::ClockTime> {
    let event = pad.sticky_event::<gst::event::Segment>(0)?;
    let segment = event.segment().downcast_ref::<gst::ClockTime>()?;
    segment.to_stream_time(buffer.pts()?)
}

fn gain_tags(gains: Gains) -> Option<gst::TagList> {
    let mut tags = gst::TagList::new();
    {
        let tags = tags.get_mut().unwrap();
        let mode = gst::TagMergeMode::Replace;
        if let Some(gain) = gains.track_gain {
            tags.add::<gst::tags::TrackGain>(&gain, mode);
        }
        if let Some(peak) = gains.track_peak {
            tags.add::<gst::tags::TrackPeak>(&peak, mode);
        }
        if let Some(gain) = gains.album_gain {
            tags.add::<gst::tags::AlbumGain>(&gain, mode);
        }
        if let Some(peak) = gains.album_peak {
            tags.add::<gst::tags::AlbumPeak>(&peak, mode);
        }
    }
    (tags.n_tags() > 0).then_some(tags)
}

// Gives rgvolume the gains of the tracks of CUE sheets, from the sheet,
// as the tags of the stream are about the whole file. They're sent before
// the first buffer of each track that reaches the filter: the stream's
// tags come before it, and at the start of the next part of a file, the
// gain changes up to a buffer late, a fraction of a second
fn add_gain_probe(pad: &gst::Pad, rg_volume_sink: gst::Pad, gapless: Arc<Mutex<Gapless>>) {
    // The track whose audio reaches the filter, ahead of the sink, and
    // whether rgvolume has its gains
    let filtered: Mutex<(Option<TrackSource>, bool)> = Mutex::default();
    pad.add_probe(
        gst::PadProbeType::BUFFER
            | gst::PadProbeType::EVENT_DOWNSTREAM
            | gst::PadProbeType::EVENT_FLUSH,
        move |pad, info| {
            let buffer = match info.data {
                Some(gst::PadProbeData::Buffer(ref buffer)) => buffer,
                Some(gst::PadProbeData::Event(ref event)) => {
                    let gapless = gapless.lock().unwrap();
                    let track = match event.type_() {
                        // The file loaded, or the one handed over
                        gst::EventType::StreamStart => {
                            gapless.handed.as_ref().or(gapless.current.as_ref())
                        }
                        // A seek stays in the track the player shows
                        gst::EventType::FlushStop => gapless.playing(),
                        _ => return gst::PadProbeReturn::Ok,
                    };
                    *filtered.lock().unwrap() = (track.cloned(), false);
                    return gst::PadProbeReturn::Ok;
                }
                _ => return gst::PadProbeReturn::Ok,
            };

            let tags = {
                let gapless = gapless.lock().unwrap();
                let mut filtered = filtered.lock().unwrap();
                // Past the end of the track, the next part of the file
                if let Some(track) = &filtered.0
                    && track
                        .end
                        .is_some_and(|end| stream_time(pad, buffer).is_some_and(|time| time >= end))
                    && gapless.next_follows(track)
                {
                    *filtered = (gapless.next.clone(), false);
                }
                if std::mem::replace(&mut filtered.1, true) {
                    return gst::PadProbeReturn::Ok;
                }
                filtered.0.as_ref().and_then(|track| track.gains)
            };
            if let Some(tags) = tags.and_then(gain_tags) {
                rg_volume_sink.send_event(gst::event::Tag::new(tags));
            }
            gst::PadProbeReturn::Ok
        },
    );
}

// What to do with a buffer at the end of a track
enum EndCut {
    Pass,
    // Cut the part of the buffer after the end, from where it is in this
    // segment
    Clip(gst::FormattedSegment<gst::ClockTime>),
    // End the stream before this buffer
    End,
}

fn end_cut(pad: &gst::Pad, buffer: &gst::BufferRef, gapless: &Mutex<Gapless>) -> EndCut {
    let Some(pts) = buffer.pts() else {
        return EndCut::Pass;
    };
    let Some(event) = pad.sticky_event::<gst::event::Segment>(0) else {
        return EndCut::Pass;
    };
    let Some(segment) = event.segment().downcast_ref::<gst::ClockTime>() else {
        return EndCut::Pass;
    };
    // Where the buffer is in the file
    let Some(start) = segment.to_stream_time(pts) else {
        return EndCut::Pass;
    };
    let stop = buffer
        .duration()
        .and_then(|duration| pts.checked_add(duration))
        .and_then(|stop| segment.to_stream_time(stop))
        .unwrap_or(start);

    let mut gapless = gapless.lock().unwrap();
    if gapless.cut || gapless.pending_loads > 0 {
        return EndCut::Pass;
    }
    let Some(playing) = gapless.playing() else {
        return EndCut::Pass;
    };
    let Some(end) = playing.end else {
        return EndCut::Pass;
    };
    if stop <= end || gapless.next_follows(playing) {
        return EndCut::Pass;
    }
    if start < end {
        let mut clip = segment.clone();
        clip.set_stop(segment.position_from_stream_time(end));
        return EndCut::Clip(clip);
    }
    gapless.cut = true;
    EndCut::End
}

// Cuts the audio after the end of a track at the src pad of the audio
// filter, unless the next track goes on from there in the same file. The
// end of the stream comes after the audio of the track, so the sink plays
// it before GstPlay reports it, and the player goes on to the next track
fn add_end_probe(pad: &gst::Pad, gapless: Arc<Mutex<Gapless>>) {
    pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_FLUSH,
        move |pad, info| {
            let cut = match info.data {
                Some(gst::PadProbeData::Buffer(ref buffer)) => end_cut(pad, buffer, &gapless),
                Some(gst::PadProbeData::Event(ref event)) => {
                    // A seek, which can go back before the end
                    if event.type_() == gst::EventType::FlushStop {
                        gapless.lock().unwrap().cut = false;
                    }
                    return gst::PadProbeReturn::Ok;
                }
                _ => return gst::PadProbeReturn::Ok,
            };

            match cut {
                EndCut::Pass => gst::PadProbeReturn::Ok,
                EndCut::Clip(segment) => {
                    let Some(audio_info) = pad
                        .current_caps()
                        .and_then(|caps| gst_audio::AudioInfo::from_caps(&caps).ok())
                    else {
                        return gst::PadProbeReturn::Ok;
                    };
                    let Some(gst::PadProbeData::Buffer(buffer)) = info.data.take() else {
                        return gst::PadProbeReturn::Ok;
                    };
                    match gst_audio::audio_buffer_clip(
                        buffer,
                        segment.upcast_ref(),
                        audio_info.rate(),
                        audio_info.bpf(),
                    ) {
                        Some(buffer) => {
                            info.data = Some(gst::PadProbeData::Buffer(buffer));
                            gst::PadProbeReturn::Ok
                        }
                        None => gst::PadProbeReturn::Drop,
                    }
                }
                // The pad refuses the buffers that come after it
                EndCut::End => {
                    debug!("Cutting the track at its end");
                    pad.push_event(gst::event::Eos::new());
                    gst::PadProbeReturn::Drop
                }
            }
        },
    );
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
}

impl GstBackend {
    pub fn new(sender: Sender<PlaybackAction>) -> Self {
        let gst_player = gst_play::Play::default();

        gst_player.set_video_track_enabled(false);

        let mut config = gst_player.config();
        config.set_position_update_interval(250);
        // The tracks of CUE sheets start where the seek takes them, while
        // other seeks would stop at the nearest seek point before
        config.set_seek_accurate(true);
        gst_player.set_config(config).unwrap();

        let res = Self {
            sender,
            gst_player,
            replaygain: GstReplayGain::new().ok(),
            start_position: Cell::default(),
            play_requested: Cell::default(),
            gapless: Arc::default(),
        };

        // There is always an audio filter, which cuts the tracks at their
        // end; the window sets the one for ReplayGain later
        if let Some(ref replaygain) = res.replaygain {
            let pad = replaygain.rg_filter_bin.static_pad("src").unwrap();
            add_end_probe(&pad, res.gapless.clone());
            let pad = replaygain.rg_filter_bin.static_pad("sink").unwrap();
            let rg_volume_sink = replaygain.rg_volume.static_pad("sink").unwrap();
            add_gain_probe(&pad, rg_volume_sink, res.gapless.clone());
        }
        res.gst_player
            .pipeline()
            .set_property("audio-filter", res.identity());

        res.setup_signals();
        res.setup_gapless();

        res
    }

    // An audio filter that changes nothing but the end of the tracks
    fn identity(&self) -> gst::Element {
        let identity = gst::ElementFactory::make_with_name("identity", None).unwrap();
        add_end_probe(&identity.static_pad("src").unwrap(), self.gapless.clone());
        identity
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
            let next = {
                let mut gapless = gapless.lock().unwrap();
                if gapless.pending_loads > 0 || gapless.handed.is_some() {
                    return None;
                }
                // playbin plays the next file from its start, after the end
                // of this one
                if gapless.cut || gapless.playing().is_some_and(|p| p.end.is_some()) {
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
                let next = gapless.next.clone()?;
                if !next.start.is_zero() {
                    return None;
                }

                // A missing track would stop the pipeline before this one ends
                if !gio::File::for_uri(&next.uri).query_exists(gio::Cancellable::NONE) {
                    return None;
                }
                gapless.handed = Some(next.clone());
                next
            };

            debug!("Handing over the next track: {}", next.id);
            playbin.set_property("uri", &next.uri);
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
            let Some(handed) = gapless.handed.as_ref() else {
                return;
            };
            let id = handed.id.clone();
            gapless.started = true;
            let generation = gapless.generation;
            drop(gapless);

            if let Err(e) = sender.try_send(PlaybackAction::TrackAdvanced(generation, id)) {
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
                        let loaded = gapless.pending_loads == 0;
                        drop(gapless);
                        // GstPlay holds its lock, so the track is played
                        // from the main thread
                        if loaded && let Err(e) = sender.try_send(PlaybackAction::TrackLoaded) {
                            error!("Failed to send TrackLoaded: {e}");
                        }
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
                            update_position(&sender, &gapless, position, false);
                        }
                    }
                    gst_play::PlayMessage::SeekDone(message) => {
                        if let Some(position) = message.position() {
                            update_position(&sender, &gapless, position, true);
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

    pub fn set_track(&self, source: Option<&TrackSource>) {
        self.start_position.set(source.map(|_| 0));
        self.play_requested.set(false);
        {
            let mut gapless = self.gapless();
            gapless.generation += 1;
            gapless.handed = None;
            gapless.started = false;
            gapless.current = source.cloned();
            gapless.advanced = None;
            gapless.cut = false;
            gapless.start_seek = false;
            if source.is_some() {
                gapless.pending_loads += 1;
                gapless.loaded_at = Some(Instant::now());
            }
        }
        // Without a track, playbin is only stopped: given no URI, it
        // checks it anyway, and GLib warns about it. As GstPlay isn't
        // loading anything, no load is counted
        match source {
            Some(source) => self.gst_player.set_uri(Some(&source.uri)),
            None => self.gst_player.stop(),
        }
    }

    // The track that playbin gets once this one is about to finish
    pub fn set_next(&self, source: Option<&TrackSource>) {
        self.gapless().next = source.cloned();
    }

    pub fn generation(&self) -> u64 {
        self.gapless().generation
    }

    // The player follows the track that was handed over, or that the
    // pipeline went on to in the same file
    pub fn confirm_switch(&self, source: &TrackSource) {
        let mut gapless = self.gapless();
        gapless.handed = None;
        gapless.started = false;
        gapless.current = Some(source.clone());
        gapless.advanced = None;
    }

    // The seeks return the position the track will start at, if it hasn't
    // played yet, which the player shows itself, as GstPlay only reports
    // positions once the track is ready
    pub fn seek(
        &self,
        position: u64,
        duration: u64,
        offset: u64,
        direction: SeekDirection,
    ) -> Option<u64> {
        let destination = seek_destination(position, duration, offset, direction)?;
        self.seek_to(seconds(destination))
    }

    pub fn seek_position(&self, position: u64) -> Option<u64> {
        self.seek_to(seconds(position))
    }

    pub fn seek_start(&self) -> Option<u64> {
        self.seek_to(gst::ClockTime::ZERO)
    }

    // The destination is from the start of the track
    fn seek_to(&self, destination: gst::ClockTime) -> Option<u64> {
        if self.start_position.get().is_some() {
            let position = destination.seconds();
            self.start_position.set(Some(position));
            Some(position)
        } else {
            let start = {
                let mut gapless = self.gapless();
                gapless.start_seek = false;
                gapless
                    .playing()
                    .map_or(gst::ClockTime::ZERO, |playing| playing.start)
            };
            self.gst_player.seek(start.saturating_add(destination));
            None
        }
    }

    // The track starts at the position once it plays, unless it's moved
    // or changed before. Getting it ready to play now, paused, could leave
    // PulseAudio waiting for it to start, and the track stuck at the
    // position, if it starts a few seconds later
    pub fn resume_at(&self, position: u64) {
        self.start_position.set(Some(position));
    }

    pub fn play(&self) {
        let (start, loading) = {
            let gapless = self.gapless();
            let start = gapless
                .current
                .as_ref()
                .map_or(gst::ClockTime::ZERO, |current| current.start);
            (start, gapless.pending_loads > 0)
        };
        // Where the track starts in its file, if not at the beginning
        let position = self
            .start_position
            .get()
            .map(|position| start.saturating_add(seconds(position)))
            .filter(|position| !position.is_zero());
        // GstPlay would drop the seek when it loads the file
        if loading && position.is_some() {
            self.play_requested.set(true);
            return;
        }

        self.start_position.take();
        self.gst_player.play();
        // GstPlay seeks once the track is ready to play
        if let Some(position) = position {
            self.gapless().start_seek = true;
            self.gst_player.seek(position);
        }
    }

    // Plays the track that was loaded, if it was asked to play meanwhile
    pub fn loaded(&self) {
        if self.gapless().pending_loads == 0 && self.play_requested.take() {
            self.play();
        }
    }

    pub fn pause(&self) {
        self.play_requested.set(false);
        self.gst_player.pause();
    }

    // Loading the track again stops it too, and replaces a track that was
    // handed over, which playbin would keep instead
    pub fn stop(&self) {
        let current = self.gapless().current.clone();
        if current.is_some() {
            self.set_track(current.as_ref());
        } else {
            self.start_position.take();
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
        let filter = match (&self.replaygain, replaygain) {
            (Some(r), ReplayGainMode::Album | ReplayGainMode::Track) => {
                r.rg_volume
                    .set_property("album-mode", replaygain == ReplayGainMode::Album);
                r.rg_filter_bin.clone()
            }
            _ => self.identity(),
        };
        self.gst_player
            .pipeline()
            .set_property("audio-filter", &filter);
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

    fn source(id: &str, start_ms: u64, end_ms: Option<u64>) -> TrackSource {
        TrackSource {
            id: id.to_string(),
            uri: "file:///music/album.flac".to_string(),
            start: gst::ClockTime::from_mseconds(start_ms),
            end: end_ms.map(gst::ClockTime::from_mseconds),
            gains: None,
        }
    }

    // What reaches the sink when buffers of 300 ms go through a filter
    // that cuts the tracks: the length of each buffer, and whether the
    // stream ended
    fn play_through_end_probe(gapless: Gapless) -> (Vec<u64>, bool) {
        gst::init().unwrap();
        let identity = gst::ElementFactory::make("identity").build().unwrap();
        let received = play_through_filter(&identity, gapless.clone());
        // The ReplayGain filter cuts them the same, from its bin's pad
        if let Ok(replaygain) = GstReplayGain::new() {
            let with_replaygain = play_through_filter(&replaygain.rg_filter_bin, gapless);
            assert_eq!(with_replaygain, received);
        }
        received
    }

    fn play_through_filter(filter: &gst::Element, gapless: Gapless) -> (Vec<u64>, bool) {
        let gapless = Arc::new(Mutex::new(gapless));

        let pipeline = gst::Pipeline::new();
        let identity = filter;
        add_end_probe(&identity.static_pad("src").unwrap(), gapless);
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .unwrap();
        pipeline.add_many([identity, &sink]).unwrap();
        identity.link(&sink).unwrap();

        let received = Arc::new(Mutex::new((Vec::new(), false)));
        let sink_received = received.clone();
        sink.static_pad("sink").unwrap().add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                let mut received = sink_received.lock().unwrap();
                match info.data {
                    Some(gst::PadProbeData::Buffer(ref buffer)) => {
                        received.0.push(buffer.duration().unwrap().mseconds());
                    }
                    Some(gst::PadProbeData::Event(ref event))
                        if event.type_() == gst::EventType::Eos =>
                    {
                        received.1 = true;
                    }
                    _ => (),
                }
                gst::PadProbeReturn::Ok
            },
        );

        let src = gst::Pad::builder(gst::PadDirection::Src).build();
        src.set_active(true).unwrap();
        src.link(&identity.static_pad("sink").unwrap()).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();

        // 1000 samples a second, of 4 bytes each
        src.push_event(gst::event::StreamStart::new("album"));
        let caps = gst_audio::AudioInfo::builder(gst_audio::AudioFormat::F32le, 1000, 1)
            .build()
            .unwrap()
            .to_caps()
            .unwrap();
        src.push_event(gst::event::Caps::new(&caps));
        src.push_event(gst::event::Segment::new(&gst::FormattedSegment::<
            gst::ClockTime,
        >::new()));
        for i in 0..5 {
            let mut buffer = gst::Buffer::with_size(300 * 4).unwrap();
            {
                let buffer = buffer.get_mut().unwrap();
                buffer.set_pts(gst::ClockTime::from_mseconds(i * 300));
                buffer.set_duration(gst::ClockTime::from_mseconds(300));
            }
            // Refused once the stream ended
            let _ = src.push(buffer);
        }

        pipeline.set_state(gst::State::Null).unwrap();
        let received = received.lock().unwrap();
        (received.0.clone(), received.1)
    }

    #[test]
    fn the_audio_after_the_end_of_a_track_is_cut() {
        let (buffers, ended) = play_through_end_probe(Gapless {
            current: Some(source("track01", 0, Some(1000))),
            ..Default::default()
        });
        assert_eq!(buffers, vec![300, 300, 300, 100]);
        assert!(ended);
    }

    #[test]
    fn the_next_part_of_the_file_plays_on() {
        let (buffers, ended) = play_through_end_probe(Gapless {
            current: Some(source("track01", 0, Some(1000))),
            next: Some(source("track02", 1000, None)),
            ..Default::default()
        });
        assert_eq!(buffers, vec![300; 5]);
        assert!(!ended);
    }

    #[test]
    fn a_track_that_plays_to_the_end_of_its_file_is_not_cut() {
        let (buffers, ended) = play_through_end_probe(Gapless {
            current: Some(source("track02", 1000, None)),
            ..Default::default()
        });
        assert_eq!(buffers, vec![300; 5]);
        assert!(!ended);
    }

    #[test]
    fn seeks_stay_within_the_track() {
        use SeekDirection::{Backwards, Forward};

        assert_eq!(seek_destination(30, 200, 10, Backwards), Some(20));
        assert_eq!(seek_destination(5, 200, 10, Backwards), Some(0));
        assert_eq!(seek_destination(30, 200, 10, Forward), Some(40));
        assert_eq!(seek_destination(195, 200, 10, Forward), Some(200));
        // A length that isn't known yet
        assert_eq!(seek_destination(30, 0, 10, Forward), None);

        // Offsets as large as an MPRIS client can send
        assert_eq!(seek_destination(30, 200, u64::MAX, Backwards), Some(0));
        assert_eq!(seek_destination(30, 200, u64::MAX, Forward), Some(200));
        assert_eq!(
            seek_destination(u64::MAX, u64::MAX, u64::MAX, Forward),
            Some(u64::MAX)
        );
    }

    #[test]
    fn large_seconds_saturate() {
        assert_eq!(seconds(90), gst::ClockTime::from_seconds(90));
        assert_eq!(seconds(u64::MAX), gst::ClockTime::MAX);
        assert_eq!(
            gst::ClockTime::from_seconds(60).saturating_add(seconds(u64::MAX)),
            gst::ClockTime::MAX
        );
    }

    #[test]
    fn positions_are_from_the_start_of_the_track() {
        let (sender, receiver) = async_channel::unbounded();
        let gapless = Mutex::new(Gapless {
            current: Some(source("track02", 60_000, Some(120_000))),
            next: Some(source("track03", 120_000, None)),
            generation: 3,
            ..Default::default()
        });

        update_position(&sender, &gapless, gst::ClockTime::from_seconds(90), false);
        assert!(matches!(
            receiver.try_recv(),
            Ok(PlaybackAction::UpdatePosition(30, false))
        ));

        // Past the end, the next part of the file takes over, until the
        // player follows it
        update_position(&sender, &gapless, gst::ClockTime::from_seconds(125), false);
        assert!(matches!(
            receiver.try_recv(),
            Ok(PlaybackAction::TrackAdvanced(3, ref id)) if id == "track03"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(PlaybackAction::UpdatePosition(5, false))
        ));
        update_position(&sender, &gapless, gst::ClockTime::from_seconds(126), false);
        assert!(matches!(
            receiver.try_recv(),
            Ok(PlaybackAction::UpdatePosition(6, false))
        ));
        assert!(receiver.try_recv().is_err());

        // Positions from before a load are dropped
        gapless.lock().unwrap().pending_loads = 1;
        update_position(&sender, &gapless, gst::ClockTime::from_seconds(10), false);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn the_track_after_a_part_of_another_file_is_not_followed() {
        let (sender, receiver) = async_channel::unbounded();
        let mut other = source("other", 0, None);
        other.uri = "file:///music/other.flac".to_string();
        let gapless = Mutex::new(Gapless {
            current: Some(source("track02", 60_000, Some(120_000))),
            next: Some(other),
            ..Default::default()
        });

        update_position(&sender, &gapless, gst::ClockTime::from_seconds(121), false);
        assert!(matches!(
            receiver.try_recv(),
            Ok(PlaybackAction::UpdatePosition(61, false))
        ));
        assert!(receiver.try_recv().is_err());
    }

    // The gain rgvolume applies after each buffer of 300 ms of a file,
    // whose stream has a track gain of -10 dB, in track mode
    fn gains_through_filter(gapless: Gapless) -> Option<Vec<f64>> {
        gst::init().unwrap();
        // rgvolume comes with the good plugins
        let replaygain = GstReplayGain::new().ok()?;
        replaygain.rg_volume.set_property("album-mode", false);
        let rg_volume_sink = replaygain.rg_volume.static_pad("sink").unwrap();
        let pad = replaygain.rg_filter_bin.static_pad("sink").unwrap();
        add_gain_probe(&pad, rg_volume_sink, Arc::new(Mutex::new(gapless)));

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
        src.link(&pad).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();

        src.push_event(gst::event::StreamStart::new("album"));
        let caps = gst_audio::AudioInfo::builder(gst_audio::AudioFormat::F32le, 1000, 1)
            .build()
            .unwrap()
            .to_caps()
            .unwrap();
        src.push_event(gst::event::Caps::new(&caps));
        src.push_event(gst::event::Segment::new(&gst::FormattedSegment::<
            gst::ClockTime,
        >::new()));
        let mut tags = gst::TagList::new();
        tags.get_mut()
            .unwrap()
            .add::<gst::tags::TrackGain>(&-10.0, gst::TagMergeMode::Append);
        src.push_event(gst::event::Tag::new(tags));

        let mut gains = Vec::new();
        for i in 0..5 {
            let mut buffer = gst::Buffer::with_size(300 * 4).unwrap();
            {
                let buffer = buffer.get_mut().unwrap();
                buffer.set_pts(gst::ClockTime::from_mseconds(i * 300));
                buffer.set_duration(gst::ClockTime::from_mseconds(300));
            }
            src.push(buffer).unwrap();
            gains.push(replaygain.rg_volume.property::<f64>("result-gain"));
        }
        pipeline.set_state(gst::State::Null).unwrap();
        Some(gains)
    }

    #[test]
    fn the_tracks_of_a_cue_sheet_have_their_own_gain() {
        let with_gain = |mut source: TrackSource, track_gain| {
            source.gains = Some(Gains {
                track_gain: Some(track_gain),
                album_gain: Some(-8.0),
                ..Default::default()
            });
            source
        };
        let Some(gains) = gains_through_filter(Gapless {
            current: Some(with_gain(source("track01", 0, Some(900)), -3.0)),
            next: Some(with_gain(source("track02", 900, None), -6.0)),
            ..Default::default()
        }) else {
            return;
        };
        // The next part of the file has its own from its first buffer
        assert_eq!(gains, vec![-3.0, -3.0, -3.0, -6.0, -6.0]);

        // A whole file has the gain of its stream
        let gains = gains_through_filter(Gapless {
            current: Some(source("track", 0, None)),
            ..Default::default()
        });
        assert_eq!(gains, Some(vec![-10.0; 5]));
    }
}
