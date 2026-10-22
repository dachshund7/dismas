// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use async_channel::Sender;
use glib::clone;
use gtk::{gio, glib, prelude::*};
use ksni::{Handle, MenuItem, TrayMethods, menu::StandardItem};
use log::{debug, error};

use crate::{
    application::ApplicationAction,
    audio::{Controller, PlaybackAction, PlaybackState, RepeatMode, Track},
    config::APPLICATION_ID,
    i18n::i18n,
    utils,
};

// What the tray shows. The controller keeps a copy, so that a tray shown
// again starts from it
#[derive(Clone, Debug, Default)]
struct Snapshot {
    playing: bool,
    track: Option<String>,
    can_go_next: bool,
}

// Translated on the main thread, as the tray runs on a thread of its own
#[derive(Debug)]
struct Labels {
    show: String,
    play: String,
    pause: String,
    previous: String,
    next: String,
    quit: String,
}

impl Labels {
    fn new() -> Self {
        Self {
            show: i18n("Show Dismas"),
            play: i18n("Play"),
            pause: i18n("Pause"),
            previous: i18n("Previous"),
            next: i18n("Next"),
            quit: i18n("_Quit"),
        }
    }
}

#[derive(Debug)]
struct DismasTray {
    sender: Sender<PlaybackAction>,
    app_sender: Sender<ApplicationAction>,
    snapshot: Snapshot,
    labels: Labels,
}

// On Wayland, a window only comes to the front with an activation token.
// The host can send it just after the click, as KDE does
fn activation_token() -> Option<String> {
    ksni::current_xdg_activation_token().or_else(|| {
        std::thread::sleep(Duration::from_millis(1));
        ksni::current_xdg_activation_token()
    })
}

impl DismasTray {
    fn send(&self, action: PlaybackAction) {
        if let Err(e) = self.sender.try_send(action) {
            error!("Unable to send a playback action: {e}");
        }
    }

    fn send_app(&self, action: ApplicationAction) {
        if let Err(e) = self.app_sender.try_send(action) {
            error!("Unable to send an application action: {e}");
        }
    }

    fn play_pause(&self) {
        if self.snapshot.playing {
            self.send(PlaybackAction::Pause);
        } else {
            self.send(PlaybackAction::Play);
        }
    }
}

impl ksni::Tray for DismasTray {
    fn id(&self) -> String {
        "dismas".into()
    }

    fn title(&self) -> String {
        "Dismas".into()
    }

    fn icon_name(&self) -> String {
        APPLICATION_ID.into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self
                .snapshot
                .track
                .clone()
                .unwrap_or_else(|| "Dismas".into()),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send_app(ApplicationAction::ToggleWindow(activation_token()));
    }

    // A middle click
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.play_pause();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let play_pause = if self.snapshot.playing {
            &self.labels.pause
        } else {
            &self.labels.play
        };
        vec![
            StandardItem {
                label: self.labels.show.clone(),
                // Only presents: the window can still have the focus while
                // the menu is open
                activate: Box::new(|this: &mut Self| {
                    this.send_app(ApplicationAction::Present(activation_token()));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: play_pause.clone(),
                activate: Box::new(|this: &mut Self| this.play_pause()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.labels.previous.clone(),
                activate: Box::new(|this: &mut Self| this.send(PlaybackAction::SkipPrevious)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.labels.next.clone(),
                enabled: self.snapshot.can_go_next,
                activate: Box::new(|this: &mut Self| this.send(PlaybackAction::SkipNext)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: self.labels.quit.clone(),
                activate: Box::new(|this: &mut Self| this.send_app(ApplicationAction::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

struct Shared {
    sender: Sender<PlaybackAction>,
    app_sender: Sender<ApplicationAction>,
    settings: gio::Settings,
    snapshot: RefCell<Snapshot>,
    // Whether the tray should be shown. Starting it takes a few calls
    // over D-Bus, and a tray that comes up after it was turned off is
    // shut down again
    wanted: Cell<bool>,
    starting: Cell<bool>,
    handle: RefCell<Option<Handle<DismasTray>>>,
}

impl Shared {
    // The tray is shown while Dismas plays in the background
    fn update_visibility(self: &Rc<Self>) {
        let wanted = self.settings.boolean("background-play");
        self.wanted.set(wanted);

        if !wanted {
            if let Some(handle) = self.handle.take() {
                glib::spawn_future_local(handle.shutdown());
            }
            return;
        }
        if self.handle.borrow().is_some() || self.starting.replace(true) {
            return;
        }

        let tray = DismasTray {
            sender: self.sender.clone(),
            app_sender: self.app_sender.clone(),
            snapshot: self.snapshot.borrow().clone(),
            labels: Labels::new(),
        };
        glib::spawn_future_local(clone!(
            #[weak(rename_to = shared)]
            self,
            async move {
                // Owning a well-known name needs a permission in Flatpak.
                // Without a tray in the desktop yet, it waits for one,
                // e.g. a panel that starts after Dismas
                let result = tray
                    .disable_dbus_name(ashpd::is_sandboxed())
                    .assume_sni_available(true)
                    .spawn()
                    .await;
                shared.starting.set(false);
                match result {
                    Ok(handle) if shared.wanted.get() => {
                        shared.handle.replace(Some(handle));
                        // What changed while it was starting
                        shared.publish();
                    }
                    Ok(handle) => handle.shutdown().await,
                    Err(err) => debug!("No tray icon: {err}"),
                }
            }
        ));
    }

    fn publish(&self) {
        let Some(handle) = self.handle.borrow().clone() else {
            return;
        };
        let snapshot = self.snapshot.borrow().clone();
        glib::spawn_future_local(async move {
            handle.update(move |tray| tray.snapshot = snapshot).await;
        });
    }
}

// A StatusNotifierItem, shown by the trays of KDE, most panels and bars,
// and GNOME with the AppIndicator extension
pub struct TrayController {
    shared: Rc<Shared>,
}

impl TrayController {
    pub fn new(sender: Sender<PlaybackAction>, app_sender: Sender<ApplicationAction>) -> Self {
        let shared = Rc::new(Shared {
            sender,
            app_sender,
            settings: utils::settings_manager(),
            snapshot: RefCell::default(),
            wanted: Cell::new(false),
            starting: Cell::new(false),
            handle: RefCell::default(),
        });

        shared.settings.connect_changed(
            Some("background-play"),
            clone!(
                #[weak]
                shared,
                move |_, _| shared.update_visibility()
            ),
        );
        shared.update_visibility();

        Self { shared }
    }

    fn set_track_text(&self, track: Option<&Track>) {
        self.shared.snapshot.borrow_mut().track =
            track.map(|track| format!("{} - {}", track.artist(), track.title()));
        self.shared.publish();
    }
}

impl Controller for TrayController {
    fn set_playback_state(&self, state: PlaybackState) {
        self.shared.snapshot.borrow_mut().playing = state == PlaybackState::Playing;
        self.shared.publish();
    }

    fn set_track(&self, track: &Track) {
        self.set_track_text(Some(track));
    }

    fn update_track(&self, track: &Track) {
        self.set_track_text(Some(track));
    }

    fn clear_track(&self) {
        self.set_track_text(None);
    }

    fn set_position(&self, _position: u64, _notify: bool) {}

    fn set_repeat_mode(&self, _repeat: RepeatMode) {}

    fn set_can_go_next(&self, can_go_next: bool) {
        // The queue changes often, e.g. while tracks are added
        let mut snapshot = self.shared.snapshot.borrow_mut();
        if std::mem::replace(&mut snapshot.can_go_next, can_go_next) == can_go_next {
            return;
        }
        drop(snapshot);
        self.shared.publish();
    }
}
