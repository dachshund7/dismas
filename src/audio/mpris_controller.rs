// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use async_channel::Sender;
use glib::clone;
use gtk::{gio, glib, prelude::*};
use log::error;
use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId};

use crate::{
    audio::{Controller, PlaybackAction, PlaybackState, RepeatMode, Track},
    config::APPLICATION_ID,
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
    can_play: Cell<bool>,
}

impl Shared {
    async fn publish_metadata(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        let metadata = self.metadata.borrow().clone();
        if let Err(err) = player.set_metadata(metadata).await {
            error!("Unable to set MPRIS metadata: {err:?}");
        }
    }

    async fn publish_playback_status(&self) {
        let Some(player) = self.player.get() else {
            return;
        };
        if let Err(err) = player.set_can_play(self.can_play.get()).await {
            error!("Unable to set MPRIS play capability: {err:?}");
        }
        if let Err(err) = player.set_playback_status(self.playback_status.get()).await {
            error!("Unable to set MPRIS playback status: {err:?}");
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
}

#[derive(Debug)]
pub struct MprisController {
    shared: Rc<Shared>,
    next_track_id: Cell<u64>,
}

impl MprisController {
    pub fn new(sender: Sender<PlaybackAction>) -> Self {
        let builder = Player::builder(APPLICATION_ID)
            .identity("Dismas")
            .desktop_entry(APPLICATION_ID)
            .can_raise(true)
            .can_play(false)
            .can_pause(true)
            .can_seek(true)
            .can_go_next(true)
            .can_go_previous(true)
            .can_set_fullscreen(false);

        let shared = Rc::new(Shared {
            player: OnceCell::new(),
            metadata: RefCell::new(Metadata::new()),
            playback_status: Cell::new(PlaybackStatus::Stopped),
            loop_status: Cell::new(LoopStatus::None),
            can_play: Cell::new(false),
        });

        glib::spawn_future_local(clone!(
            #[weak]
            shared,
            #[strong]
            sender,
            async move {
                match builder.build().await {
                    Err(err) => error!("Failed to create MPRIS server: {err:?}"),
                    Ok(player) => {
                        setup_signals(sender, &player);
                        let mpris_task = player.run();
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

    fn track_metadata(&self, track: &Track) -> Metadata {
        let mut metadata = Metadata::new();

        // Clients pass the id back when setting the position; a new one
        // for each track, as the same track can be in the queue more than
        // once
        let track_id = self.next_track_id.get();
        self.next_track_id.set(track_id + 1);
        match TrackId::try_from(format!("/io/github/dachshund7/Dismas/Track/{track_id}")) {
            Ok(id) => metadata.set_trackid(Some(id)),
            Err(e) => error!("Invalid MPRIS track id: {e}"),
        }

        metadata.set_artist(Some(vec![track.artist()]));
        metadata.set_title(Some(track.title()));
        metadata.set_album(Some(track.album()));

        let length = Time::from_secs(track.duration() as i64);
        metadata.set_length(Some(length));

        // MPRIS should really support passing a bytes buffer for the cover
        // art, instead of requiring this ridiculous charade
        if let Some(cache) = track.cover_cache() {
            let file = gio::File::for_path(cache);
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
}

impl Controller for MprisController {
    fn set_playback_state(&self, state: PlaybackState) {
        let status = match state {
            PlaybackState::Playing => PlaybackStatus::Playing,
            PlaybackState::Paused => PlaybackStatus::Paused,
            PlaybackState::Stopped => PlaybackStatus::Stopped,
        };
        self.shared.playback_status.set(status);
        self.shared.can_play.set(true);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_playback_status().await }
        ));
    }

    fn set_track(&self, track: &Track) {
        self.shared.metadata.replace(self.track_metadata(track));
        self.shared.can_play.set(true);

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move {
                shared.publish_metadata().await;
                shared.publish_playback_status().await;
            }
        ));
    }

    fn update_track(&self, track: &Track) {
        self.shared.metadata.replace(self.track_metadata(track));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self.shared,
            async move { shared.publish_metadata().await }
        ));
    }

    fn set_position(&self, position: u64, notify: bool) {
        let pos = Time::from_secs(position as i64);
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
}

fn setup_signals(sender: Sender<PlaybackAction>, mpris: &Player) {
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
        move |_| {
            if let Err(e) = sender.try_send(PlaybackAction::Pause) {
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

    mpris.connect_set_loop_status(clone!(
        #[strong]
        sender,
        move |_, status| {
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

    mpris.connect_set_position(clone!(
        #[strong]
        sender,
        move |player, track_id, position| {
            // The position is for the track the client last saw, which
            // might not be playing anymore
            if player.metadata().trackid().as_ref() != Some(track_id) || position.is_negative() {
                return;
            }

            let position = position.as_secs() as u64;
            if let Err(e) = sender.try_send(PlaybackAction::SeekTo(position)) {
                error!("Unable to send SeekTo({position}): {e}");
            }
        }
    ));

    mpris.connect_seek(clone!(
        #[strong]
        sender,
        move |_, offset| {
            let offset = offset.as_secs();
            if let Err(e) = sender.try_send(PlaybackAction::Seek(offset)) {
                error!("Unable to send Seek({offset}): {e}");
            }
        }
    ));
}
