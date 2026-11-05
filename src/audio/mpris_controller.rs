// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::Duration,
};

use async_channel::Sender;
use futures::FutureExt;
use glib::clone;
use gtk::{gio, glib, prelude::*};
use log::error;
use mpris_server::{
    LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId, Volume,
    zbus::zvariant::{Dict, Value},
};

use crate::{
    application::ApplicationAction,
    audio::{Controller, PlaybackAction, PlaybackState, RepeatMode, Track},
    config::APPLICATION_ID,
    utils,
};

// What the desktop is told about the player. The server starts after the
// player, which can already have a track by then, so it's kept to be
// told once the server is up
#[derive(Debug)]
struct Shared {
    player: OnceCell<Player>,
    metadata: RefCell<Metadata>,
    playback_status: Cell<PlaybackStatus>,
    loop_status: Cell<LoopStatus>,
    shuffle: Cell<bool>,
    can_shuffle: Cell<bool>,
    fullscreen: Cell<bool>,
    can_set_fullscreen: Cell<bool>,
    has_track: Cell<bool>,
    can_go_next: Cell<bool>,
    can_go_previous: Cell<bool>,
    position: Cell<Time>,
    volume: Cell<Volume>,
}

impl Shared {
    async fn publish_metadata(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        let metadata = without_nuls(self.metadata.borrow().clone());
        if let Err(err) = player.set_metadata(metadata).await {
            error!("Unable to set MPRIS metadata: {err:?}");
        }
    }

    async fn publish_playback_status(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        if let Err(err) = player.set_playback_status(self.playback_status.get()).await {
            error!("Unable to set MPRIS playback status: {err:?}");
        }
    }

    // What clients can ask for, which their buttons follow. Each value is
    // read just before it's published: this waits between them, and another
    // publish can run in the meantime, whose newer values a value read
    // earlier would then replace
    async fn publish_capabilities(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        // Without a current track, Play starts the track that comes next
        let can_play = self.has_track.get() || self.can_go_next.get();
        if let Err(err) = player.set_can_play(can_play).await {
            error!("Unable to set MPRIS play capability: {err:?}");
        }
        if let Err(err) = player.set_can_pause(self.has_track.get()).await {
            error!("Unable to set MPRIS pause capability: {err:?}");
        }
        if let Err(err) = player.set_can_seek(self.has_track.get()).await {
            error!("Unable to set MPRIS seek capability: {err:?}");
        }
        if let Err(err) = player.set_can_go_next(self.can_go_next.get()).await {
            error!("Unable to set MPRIS next capability: {err:?}");
        }
        let can_go_previous = self.can_go_previous.get();
        if let Err(err) = player.set_can_go_previous(can_go_previous).await {
            error!("Unable to set MPRIS previous capability: {err:?}");
        }
    }

    async fn publish_loop_status(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        if let Err(err) = player.set_loop_status(self.loop_status.get()).await {
            error!("Unable to set MPRIS loop status: {err:?}");
        }
    }

    async fn publish_shuffle(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        if let Err(err) = player.set_shuffle(self.shuffle.get()).await {
            error!("Unable to set MPRIS shuffle: {err:?}");
        }
    }

    async fn publish_fullscreen(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        let can_set_fullscreen = self.can_set_fullscreen.get();
        if let Err(err) = player.set_can_set_fullscreen(can_set_fullscreen).await {
            error!("Unable to set MPRIS fullscreen capability: {err:?}");
        }
        if let Err(err) = player.set_fullscreen(self.fullscreen.get()).await {
            error!("Unable to set MPRIS fullscreen: {err:?}");
        }
    }

    async fn publish_volume(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        if let Err(err) = player.set_volume(self.volume.get()).await {
            error!("Unable to set MPRIS volume: {err:?}");
        }
    }

    fn store_requested_volume(self: &Rc<Self>, volume: Volume) {
        self.volume.set(volume);
        let shared = self.clone();
        publish_at_once(async move { shared.publish_volume().await });
    }

    fn store_requested_loop_status(self: &Rc<Self>, status: LoopStatus) {
        self.loop_status.set(status);
        let shared = self.clone();
        publish_at_once(async move { shared.publish_loop_status().await });
    }

    fn store_requested_shuffle(self: &Rc<Self>, shuffle: bool) {
        self.shuffle.set(shuffle);
        let shared = self.clone();
        publish_at_once(async move { shared.publish_shuffle().await });
    }
}

// Stores a property that a client set before its call returns. zbus
// announces a property as soon as its setter returns, with the value the
// server has by then, and the player only applies the change later:
// announcing the old value would send clients back to it. The setters of
// `Player` store the value before they first wait, so one poll is enough
// for that. The rest is left to finish, as dropping it could cut off a
// message that's being sent
fn publish_at_once(publish: impl Future<Output = ()> + 'static) {
    let mut publish = Box::pin(publish);
    if publish.as_mut().now_or_never().is_none() {
        glib::spawn_future_local(publish);
    }
}

// The metadata with no NUL in its texts, see `utils::without_nul()`. All of
// the metadata passes through here on its way to the bus, so a new field is
// covered without anyone having to remember it. The texts come from tags,
// CUE sheets and the JSON of the Work Hierarchy tag, so stripping them where
// they're read would take several places instead of this one
fn without_nuls(metadata: Metadata) -> Metadata {
    let values = Value::from(metadata)
        .downcast::<Dict>()
        .and_then(HashMap::<String, Value>::try_from)
        .unwrap_or_default();
    let mut clean = Metadata::new();
    for (key, value) in values {
        clean.set_value(&key, value_without_nuls(value));
    }
    clean
}

// A value that's neither a text nor a list of texts is left as it is. Every
// list in the metadata is one of texts; another one would be dropped rather
// than sent unchecked
fn value_without_nuls(value: Value<'static>) -> Option<Value<'static>> {
    match value {
        Value::Str(text) if text.contains('\0') => {
            Some(Value::from(utils::without_nul(text.as_str())))
        }
        Value::Array(texts) => {
            let texts = Vec::<String>::try_from(texts).ok()?;
            let texts: Vec<String> = texts.into_iter().map(utils::without_nul).collect();
            Some(Value::from(texts))
        }
        value => Some(value),
    }
}

// An MPRIS time, which counts microseconds in an i64
fn time(duration: Duration) -> Time {
    Time::from_micros(i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
}

// The length of a track for MPRIS, left out when it isn't known, which a
// length of 0 would say it is
fn track_length(length: Duration) -> Option<Time> {
    (!length.is_zero()).then(|| time(length))
}

// Whether a client's position is one to go to. MPRIS asks to do nothing
// past the end of the track; in a track whose length isn't known, the
// player goes as far as it can
fn is_within_track(position: Time, length: Option<Time>) -> bool {
    !position.is_negative() && length.is_none_or(|length| position <= length)
}

// The volume a client asks for, within what the volume slider shows. A
// program can send anything over D-Bus, and NaN gets through `clamp`
fn requested_volume(volume: Volume) -> Option<f64> {
    (!volume.is_nan()).then(|| volume.clamp(0.0, 1.0))
}

#[derive(Debug)]
pub(crate) struct MprisController {
    shared: Rc<Shared>,
    next_track_id: Cell<u64>,
}

impl MprisController {
    pub(crate) fn new(
        sender: Sender<PlaybackAction>,
        app_sender: Sender<ApplicationAction>,
    ) -> Self {
        let builder = Player::builder(APPLICATION_ID)
            .identity("Dismas")
            .desktop_entry(APPLICATION_ID)
            .can_quit(true)
            .can_raise(true)
            .can_play(false)
            .can_pause(false)
            .can_seek(false)
            .can_go_next(false)
            .can_go_previous(false)
            .can_set_fullscreen(false);

        let shared = Rc::new(Shared {
            player: OnceCell::new(),
            metadata: RefCell::new(Metadata::new()),
            playback_status: Cell::new(PlaybackStatus::Stopped),
            loop_status: Cell::new(LoopStatus::None),
            shuffle: Cell::new(false),
            can_shuffle: Cell::new(false),
            fullscreen: Cell::new(false),
            can_set_fullscreen: Cell::new(false),
            has_track: Cell::new(false),
            can_go_next: Cell::new(false),
            can_go_previous: Cell::new(false),
            position: Cell::new(Time::ZERO),
            volume: Cell::new(1.0),
        });

        glib::spawn_future_local(clone!(
            #[weak]
            shared,
            async move {
                match builder.build().await {
                    Err(err) => error!("Failed to create MPRIS server: {err:?}"),
                    Ok(player) => {
                        setup_signals(sender, app_sender, &shared, &player);
                        let mpris_task = player.run();
                        // Not published, as it changes too often: read when asked
                        player.set_position(shared.position.get());
                        let _ = shared.player.set(player);

                        // What changed while the server was starting; the
                        // task of the server is to be awaited right away
                        glib::spawn_future_local(clone!(
                            #[weak]
                            shared,
                            async move {
                                shared.publish_metadata().await;
                                shared.publish_playback_status().await;
                                shared.publish_loop_status().await;
                                shared.publish_shuffle().await;
                                shared.publish_fullscreen().await;
                                shared.publish_capabilities().await;
                                shared.publish_volume().await;
                            }
                        ));

                        mpris_task.await;
                    }
                }
            }
        ));

        Self {
            shared,
            next_track_id: Cell::new(0),
        }
    }

    // Clients pass the id back when setting the position, and tell tracks
    // apart by it: a new one each time a track becomes current, as the same
    // track can be in the queue more than once
    fn new_track_id(&self) -> Option<TrackId> {
        let track_id = self.next_track_id.get();
        self.next_track_id.set(track_id + 1);
        TrackId::try_from(format!("/io/github/dachshund7/Dismas/Track/{track_id}"))
            .inspect_err(|e| error!("Invalid MPRIS track id: {e}"))
            .ok()
    }

    fn track_metadata(&self, track: &Track, id: Option<TrackId>) -> Metadata {
        let mut metadata = Metadata::new();
        metadata.set_trackid(id);

        // Desktop widgets show the title and the artists, so the artists
        // name the composer or the performers too, as the player does
        metadata.set_artist(Some(track.credited_artists()));
        metadata.set_title(Some(track.title()));
        metadata.set_album(Some(track.album()));
        metadata.set_url(Some(track.audio_uri()));

        metadata.set_length(track_length(track.length()));

        // For the bars and widgets that can show more
        let credits = track.credit_details();
        // The start of a recording made over several months
        let date = credits.year_date().and_then(|d| d.split('/').next());
        metadata.set_content_created(date);
        let names = |names: Vec<String>| Some(names).filter(|n| !n.is_empty());
        metadata.set_composer(names(credits.composers));
        metadata.set_lyricist(names(credits.lyricists));
        metadata.set_genre(names(credits.genres));
        metadata.set_album_artist(credits.album_artist.map(|a| vec![a]));
        let number = |n: Option<u32>| n.and_then(|n| i32::try_from(n).ok());
        metadata.set_track_number(number(credits.track_number));
        metadata.set_disc_number(number(credits.disc_number));

        // MPRIS should really support passing a bytes buffer for the cover
        // art, instead of requiring this ridiculous charade
        if let Some(file) = track.cover_cache_file() {
            match file.query_info(
                "standard::type",
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            ) {
                Ok(info) if info.file_type() == gio::FileType::Regular => {
                    metadata.set_art_url(Some(file.uri()));
                }
                _ => metadata.set_art_url(None::<String>),
            }
        }

        metadata
    }

    fn publish_track(&self) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move {
                shared.publish_metadata().await;
                shared.publish_capabilities().await;
            }
        ));
    }
}

impl Controller for MprisController {
    fn set_playback_state(&self, state: PlaybackState) {
        let status = match state {
            PlaybackState::Playing => PlaybackStatus::Playing,
            PlaybackState::Paused => PlaybackStatus::Paused,
            PlaybackState::Stopped => PlaybackStatus::Stopped,
        };
        self.shared.playback_status.set(status);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_playback_status().await }
        ));
    }

    fn set_track(&self, track: &Track) {
        let id = self.new_track_id();
        self.shared.metadata.replace(self.track_metadata(track, id));
        self.shared.has_track.set(true);
        self.publish_track();
    }

    fn clear_track(&self) {
        self.shared.metadata.replace(Metadata::new());
        self.shared.has_track.set(false);
        self.set_position(Duration::ZERO, false);
        self.publish_track();
    }

    fn update_track(&self, track: &Track) {
        // It's still the same track, whose tags were read again
        let id = self.shared.metadata.borrow().trackid();
        self.shared.metadata.replace(self.track_metadata(track, id));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_metadata().await }
        ));
    }

    fn set_position(&self, position: Duration, notify: bool) {
        let pos = time(position);
        self.shared.position.set(pos);
        if let Some(player) = self.shared.player.get() {
            player.set_position(pos);
        }
        if notify {
            glib::spawn_future_local(clone!(
                #[weak(rename_to = shared)]
                self.shared,
                async move {
                    if let Some(player) = shared.player.get()
                        && let Err(err) = player.seeked(pos).await
                    {
                        error!("Unable to emit MPRIS Seeked: {err:?}");
                    }
                }
            ));
        }
    }

    fn set_can_go_next(&self, can_go_next: bool) {
        // The queue changes often, e.g. while tracks are added
        if self.shared.can_go_next.replace(can_go_next) == can_go_next {
            return;
        }

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_capabilities().await }
        ));
    }

    fn set_can_go_previous(&self, can_go_previous: bool) {
        // Asked at every update of the position
        if self.shared.can_go_previous.replace(can_go_previous) == can_go_previous {
            return;
        }

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_capabilities().await }
        ));
    }

    fn set_repeat_mode(&self, repeat: RepeatMode) {
        let status = match repeat {
            RepeatMode::Consecutive => LoopStatus::None,
            RepeatMode::RepeatOne => LoopStatus::Track,
            RepeatMode::RepeatAll => LoopStatus::Playlist,
        };
        self.shared.loop_status.set(status);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_loop_status().await }
        ));
    }

    fn set_shuffle(&self, shuffled: bool, can_change: bool) {
        self.shared.can_shuffle.set(can_change);
        // Asked at every change to what the queue can do
        if self.shared.shuffle.replace(shuffled) == shuffled {
            return;
        }

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_shuffle().await }
        ));
    }

    fn set_fullscreen(&self, fullscreen: bool, can_change: bool) {
        self.shared.fullscreen.set(fullscreen);
        self.shared.can_set_fullscreen.set(can_change);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_fullscreen().await }
        ));
    }

    fn set_volume(&self, volume: f64) {
        self.shared.volume.set(volume);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_volume().await }
        ));
    }
}

fn setup_signals(
    sender: Sender<PlaybackAction>,
    app_sender: Sender<ApplicationAction>,
    shared: &Rc<Shared>,
    mpris: &Player,
) {
    mpris.connect_play_pause(clone!(
        #[strong]
        sender,
        move |player| {
            match player.playback_status() {
                PlaybackStatus::Paused | PlaybackStatus::Stopped => {
                    if let Err(e) = sender.try_send(PlaybackAction::Play) {
                        error!("Unable to send Play: {e}");
                    }
                }
                PlaybackStatus::Playing => {
                    if let Err(e) = sender.try_send(PlaybackAction::Pause) {
                        error!("Unable to send Pause: {e}");
                    }
                }
            }
        }
    ));

    mpris.connect_play(clone!(
        #[strong]
        sender,
        move |_| {
            if let Err(e) = sender.try_send(PlaybackAction::Play) {
                error!("Unable to send Play: {e}");
            }
        }
    ));

    mpris.connect_stop(clone!(
        #[strong]
        sender,
        move |_| {
            if let Err(e) = sender.try_send(PlaybackAction::Stop) {
                error!("Unable to send Stop: {e}");
            }
        }
    ));

    mpris.connect_pause(clone!(
        #[strong]
        sender,
        move |player| {
            if player.can_pause()
                && let Err(e) = sender.try_send(PlaybackAction::Pause)
            {
                error!("Unable to send Pause: {e}");
            }
        }
    ));

    mpris.connect_previous(clone!(
        #[strong]
        sender,
        move |_| {
            if let Err(e) = sender.try_send(PlaybackAction::SkipPrevious) {
                error!("Unable to send SkipPrevious: {e}");
            }
        }
    ));

    mpris.connect_next(clone!(
        #[strong]
        sender,
        move |_| {
            if let Err(e) = sender.try_send(PlaybackAction::SkipNext) {
                error!("Unable to send SkipNext: {e}");
            }
        }
    ));

    mpris.connect_raise(clone!(
        #[strong]
        sender,
        move |_| {
            if let Err(e) = sender.try_send(PlaybackAction::Raise) {
                error!("Unable to send Raise: {e}");
            }
        }
    ));

    mpris.connect_quit(clone!(
        #[strong]
        app_sender,
        move |_| {
            if let Err(e) = app_sender.try_send(ApplicationAction::Quit) {
                error!("Unable to send Quit: {e}");
            }
        }
    ));

    mpris.connect_set_loop_status(clone!(
        #[strong]
        sender,
        #[weak]
        shared,
        move |_, status| {
            shared.store_requested_loop_status(status);
            let mode = match status {
                LoopStatus::None => RepeatMode::Consecutive,
                LoopStatus::Track => RepeatMode::RepeatOne,
                LoopStatus::Playlist => RepeatMode::RepeatAll,
            };

            if let Err(e) = sender.try_send(PlaybackAction::Repeat(mode)) {
                error!("Unable to send Repeat({mode}): {e}");
            }
        }
    ));

    // Not stored, unlike what the player changes by itself: the window
    // manager can refuse, and the window says when it's done
    mpris.connect_set_fullscreen(clone!(
        #[strong]
        app_sender,
        move |player, fullscreen| {
            if player.can_set_fullscreen()
                && let Err(e) = app_sender.try_send(ApplicationAction::SetFullscreen(fullscreen))
            {
                error!("Unable to send SetFullscreen({fullscreen}): {e}");
            }
        }
    ));

    mpris.connect_set_shuffle(clone!(
        #[weak]
        shared,
        move |_, shuffle| {
            // Left out before it's stored, so that zbus announces what's
            // still true
            if !shared.can_shuffle.get() {
                return;
            }

            shared.store_requested_shuffle(shuffle);
            if let Err(e) = app_sender.try_send(ApplicationAction::SetQueueShuffled(shuffle)) {
                error!("Unable to send SetQueueShuffled({shuffle}): {e}");
            }
        }
    ));

    mpris.connect_set_volume(clone!(
        #[strong]
        sender,
        #[weak]
        shared,
        move |_, volume| {
            let Some(volume) = requested_volume(volume) else {
                return;
            };
            shared.store_requested_volume(volume);
            if let Err(e) = sender.try_send(PlaybackAction::SetVolume(volume)) {
                error!("Unable to send SetVolume({volume}): {e}");
            }
        }
    ));

    mpris.connect_set_position(clone!(
        #[strong]
        sender,
        move |player, track_id, position| {
            // The position is for the track the client last saw, which
            // might not be playing anymore
            let metadata = player.metadata();
            if metadata.trackid().as_ref() != Some(track_id)
                || !is_within_track(position, metadata.length())
            {
                return;
            }

            let position = Duration::from_micros(position.as_micros().unsigned_abs());
            if let Err(e) = sender.try_send(PlaybackAction::SeekTo(position)) {
                error!("Unable to send SeekTo({position:?}): {e}");
            }
        }
    ));

    mpris.connect_seek(move |player, offset| {
        if !player.can_seek() {
            return;
        }

        let offset = offset.as_micros();
        if let Err(e) = sender.try_send(PlaybackAction::Seek(offset)) {
            error!("Unable to send Seek({offset}): {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_times_saturate() {
        assert_eq!(time(Duration::from_secs(90)), Time::from_secs(90));
        assert_eq!(time(Duration::MAX), Time::from_micros(i64::MAX));
    }

    #[test]
    fn times_keep_the_parts_of_a_second() {
        let time = time(Duration::from_millis(1500));
        assert_eq!(time, Time::from_micros(1_500_000));
    }

    #[test]
    fn an_unknown_length_is_left_out() {
        assert_eq!(track_length(Duration::ZERO), None);
        let length = Duration::from_millis(90_500);
        assert_eq!(track_length(length), Some(time(length)));
    }

    #[test]
    fn positions_past_the_end_of_a_track_are_left_alone() {
        let seconds = |seconds| time(Duration::from_secs(seconds));
        let length = Some(seconds(40));
        assert!(is_within_track(seconds(30), length));
        assert!(is_within_track(seconds(40), length));
        assert!(!is_within_track(seconds(41), length));
        assert!(!is_within_track(Time::from_micros(-1), length));
        assert!(is_within_track(seconds(9000), None));
        assert!(!is_within_track(Time::from_micros(-1), None));
    }

    // Whichever field gets a text, as a text or in a list of them
    #[test]
    fn no_nul_reaches_the_bus() {
        let mut metadata = Metadata::new();
        metadata.set_title(Some("Die\0 Forelle"));
        metadata.set_lyrics(Some("In einem Bächlein\0"));
        metadata.set_content_created(Some("1817\0"));
        metadata.set_artist(Some(["Fischer\0-Dieskau", "Moore"]));
        metadata.set_composer(Some(["\0Schubert"]));
        metadata.set_genre(Some(["Lied\0", "Klassik\0"]));

        let clean = without_nuls(metadata);
        assert_eq!(clean.title(), Some("Die Forelle"));
        assert_eq!(clean.lyrics(), Some("In einem Bächlein"));
        assert_eq!(clean.content_created().as_deref(), Some("1817"));
        let artists = ["Fischer-Dieskau".to_string(), "Moore".to_string()];
        assert_eq!(clean.artist().as_deref(), Some(&artists[..]));
        assert_eq!(clean.composer(), Some(vec!["Schubert".to_string()]));
        let genres = ["Lied".to_string(), "Klassik".to_string()];
        assert_eq!(clean.genre().as_deref(), Some(&genres[..]));
    }

    #[test]
    fn metadata_without_nuls_stays_the_same() {
        let mut metadata = Metadata::new();
        metadata.set_trackid(TrackId::try_from("/io/github/dachshund7/Dismas/Track/7").ok());
        metadata.set_title(Some("Die Forelle"));
        metadata.set_artist(Some(["Fischer-Dieskau", "Moore"]));
        metadata.set_length(Some(Time::from_secs(90)));
        metadata.set_track_number(Some(4));
        metadata.set_user_rating(Some(1.0));
        assert_eq!(without_nuls(metadata.clone()), metadata);
    }

    #[test]
    fn requested_volumes_stay_in_the_sliders_range() {
        assert_eq!(requested_volume(0.3), Some(0.3));
        assert_eq!(requested_volume(-1.0), Some(0.0));
        assert_eq!(requested_volume(2.0), Some(1.0));
        assert_eq!(requested_volume(f64::INFINITY), Some(1.0));
        assert_eq!(requested_volume(f64::NAN), None);
    }
}
