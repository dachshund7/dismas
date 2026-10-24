// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use adw::prelude::AdwDialogExt;
use adw::subclass::prelude::*;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use ashpd::{WindowIdentifier, desktop::background::Background};
use async_channel::Receiver;
use glib::clone;
use gtk::{gdk, gio, glib, prelude::*};
use log::{debug, warn};

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use crate::x11_stacking::{self, Visibility};
use crate::{
    audio::AudioPlayer,
    config::{APPLICATION_ID, VERSION},
    i18n::i18n,
    playlists,
    resume_point::{ResumePoint, ResumePoints},
    utils,
    window::Window,
};

// The tokens are XDG activation tokens, which bring the window to the
// front on Wayland
pub enum ApplicationAction {
    Present(Option<String>),
    // Closes the window if it has the focus, and presents it otherwise
    ToggleWindow(Option<String>),
    Quit,
}

// How often the position is saved while playing, in case Dismas doesn't
// get to save it when it quits
const RESUME_POINT_INTERVAL: Duration = Duration::from_secs(30);

mod imp {
    use super::*;

    #[derive(Debug)]
    pub struct Application {
        pub player: Rc<AudioPlayer>,
        pub receiver: RefCell<Option<Receiver<ApplicationAction>>>,
        pub background_hold: RefCell<Option<gio::ApplicationHoldGuard>>,
        pub settings: gio::Settings,
        pub resume_point_pending: Cell<bool>,
        pub resume_point_saved: Cell<Instant>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Application {
        const NAME: &'static str = "DismasApplication";
        type Type = super::Application;
        type ParentType = adw::Application;

        fn new() -> Self {
            let (sender, r) = async_channel::unbounded();
            let receiver = RefCell::new(Some(r));

            Self {
                player: AudioPlayer::new(sender),
                receiver,
                background_hold: RefCell::default(),
                settings: utils::settings_manager(),
                resume_point_pending: Cell::new(false),
                resume_point_saved: Cell::new(Instant::now()),
            }
        }
    }

    impl ObjectImpl for Application {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.setup_channel();
            obj.setup_gactions();
            obj.setup_settings();
            obj.setup_resume_point();

            obj.set_accels_for_action("app.quit", &["<primary>q"]);

            obj.set_accels_for_action("queue.clear", &["<primary>L"]);
            obj.set_accels_for_action("queue.toggle", &["F9"]);
            obj.set_accels_for_action("queue.find", &["<primary>F"]);
            obj.set_accels_for_action("queue.shuffle", &["<primary>r"]);

            // Ctrl+Left and Ctrl+Right seek, see `Window::setup_key_shortcuts()`
            obj.set_accels_for_action("win.volume-up", &["<primary>Up"]);
            obj.set_accels_for_action("win.volume-down", &["<primary>Down"]);
            obj.set_accels_for_action("win.previous", &["<Alt>Left"]);
            obj.set_accels_for_action("win.next", &["<Alt>Right"]);
            obj.set_accels_for_action("win.copy", &["<primary>c"]);
            obj.set_accels_for_action("win.track-info", &["<primary>i"]);
            obj.set_accels_for_action("win.preferences", &["<primary>comma"]);
            // In the order of the buttons of the pages
            obj.set_accels_for_action("win.page::player", &["<primary>1"]);
            obj.set_accels_for_action("win.page::library", &["<primary>2", "<primary>g"]);
            obj.set_accels_for_action("win.page::playlists", &["<primary>3"]);
        }
    }

    impl ApplicationImpl for Application {
        fn startup(&self) {
            self.parent_startup();

            gtk::Window::set_default_icon_name(APPLICATION_ID);

            // Logging out ends Dismas this way, which quits, so that it
            // saves where playback was
            glib_unix::unix_signal_add_local_once(
                libc::SIGTERM,
                clone!(
                    #[weak(rename_to = app)]
                    self.obj(),
                    move || app.quit()
                ),
            );
        }

        fn shutdown(&self) {
            // Quitting doesn't close the window, which saves its state then
            for window in self.obj().windows() {
                if let Ok(window) = window.downcast::<Window>() {
                    window.save_window_state();
                }
            }
            self.obj().save_resume_point();
            // The settings are written in the background, which would
            // stop with the process
            gio::Settings::sync();

            self.parent_shutdown();
        }

        fn activate(&self) {
            debug!("Application::activate");

            self.obj().present_main_window(true, None);
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            debug!("Application::open");

            // The files take the place of the queue of the last session
            self.obj()
                .present_main_window(false, None)
                .open_files(files);
        }
    }

    impl GtkApplicationImpl for Application {}
    impl AdwApplicationImpl for Application {}
}

glib::wrapper! {
    pub struct Application(ObjectSubclass<imp::Application>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for Application {
    fn default() -> Self {
        glib::Object::builder::<Application>()
            .property("application-id", APPLICATION_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .property("resource-base-path", "/io/github/dachshund7/Dismas")
            .build()
    }
}

// Whether a click on the tray icon closes the window, rather than bringing
// it up: when it has the focus, or when it's on screen and nothing covers
// it, as moving to the tray can give the focus to another window when the
// focus follows the pointer. GTK knows neither about desktops nor about
// the windows of other apps, so on X11 the window manager is asked
fn closes_on_click(window: &Window) -> bool {
    if !window.is_visible() {
        return false;
    }
    let Some(surface) = window.surface() else {
        return false;
    };
    // Wayland compositors suspend the windows of hidden workspaces
    if surface
        .downcast_ref::<gdk::Toplevel>()
        .is_some_and(|toplevel| {
            toplevel
                .state()
                .intersects(gdk::ToplevelState::MINIMIZED | gdk::ToplevelState::SUSPENDED)
        })
    {
        return false;
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if let Some(x11_surface) = surface.downcast_ref::<gdk_x11::X11Surface>() {
        let xid = u32::try_from(x11_surface.xid()).unwrap_or_default();
        return match x11_stacking::visibility(xid) {
            Some(Visibility::Hidden) => false,
            Some(Visibility::Covered) => window.is_active(),
            Some(Visibility::Uncovered) => true,
            // Without the order of the windows
            None => !on_other_desktop(&surface),
        };
    }
    // On Wayland, apps aren't told which windows cover theirs
    true
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn on_other_desktop(surface: &gdk::Surface) -> bool {
    let Some(surface) = surface.downcast_ref::<gdk_x11::X11Surface>() else {
        return false;
    };
    let Some(display) = surface.display().downcast::<gdk_x11::X11Display>().ok() else {
        return false;
    };
    // Sticky windows are on all desktops
    let desktop = surface.desktop();
    desktop != u32::MAX && desktop != display.screen().current_desktop()
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn on_other_desktop(_surface: &gdk::Surface) -> bool {
    false
}

impl Application {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn player(&self) -> Rc<AudioPlayer> {
        self.imp().player.clone()
    }

    // Whether closing the window keeps the music playing
    pub(crate) fn plays_in_background(&self) -> bool {
        self.imp().background_hold.borrow().is_some()
    }

    fn setup_settings(&self) {
        self.imp().settings.connect_changed(
            Some("background-play"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |settings, _| {
                    let background_play = settings.boolean("background-play");
                    debug!("GSettings:background-play: {background_play}");
                    if background_play {
                        this.request_background();
                    } else {
                        debug!("Dropping background hold");
                        this.imp().background_hold.replace(None);
                        // A hidden window would keep Dismas running
                        if let Some(window) = this.active_window()
                            && !window.is_visible()
                        {
                            window.close();
                        }
                    }
                }
            ),
        );

        let _dummy = self.imp().settings.boolean("background-play");
    }

    // Saves where playback was when it pauses, stops, goes to another track,
    // or the tracks move, and now and then while playing
    fn setup_resume_point(&self) {
        let player = self.player();
        let state = player.state();
        for property in ["playing", "track"] {
            state.connect_notify_local(
                Some(property),
                clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |_, _| app.queue_resume_point_save()
                ),
            );
        }
        state.connect_notify_local(
            Some("position"),
            clone!(
                #[weak(rename_to = app)]
                self,
                move |state, _| {
                    let saved = app.imp().resume_point_saved.get();
                    if state.playing() && saved.elapsed() >= RESUME_POINT_INTERVAL {
                        app.queue_resume_point_save();
                    }
                }
            ),
        );
        player.queue().model().connect_items_changed(clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _, _, _| app.queue_resume_point_save()
        ));
    }

    // Changes come in bursts, e.g. pausing, going to the next track and
    // playing again; they're saved once, after the queue has changed
    fn queue_resume_point_save(&self) {
        if self.imp().resume_point_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = app)]
            self,
            move || app.save_resume_point()
        ));
    }

    // Saves the point of the selected playlist. Without a track, the last
    // point stays: it can't be found in the playlist anymore, so it's
    // harmless. The window saves it right before switching playlists, as
    // the one at idle would be the point of the next playlist
    pub(crate) fn save_resume_point(&self) {
        let imp = self.imp();
        imp.resume_point_pending.set(false);
        imp.resume_point_saved.set(Instant::now());

        let Some((track, position)) = imp.player.resume_point() else {
            return;
        };
        let Some(index) = imp.player.queue().position(&track) else {
            return;
        };
        // A deleted playlist could still be playing, while the queue
        // loads; its point would come back
        let playlist: String = imp.settings.string("selected-playlist").into();
        if !playlist.is_empty() && !playlists::exists(&playlist) {
            return;
        }
        let point = ResumePoint {
            uri: track.uri(),
            index,
            position: position.try_into().unwrap_or(u32::MAX),
        };
        ResumePoints::update(&imp.settings, |points| points.set(&playlist, point));
    }

    fn setup_channel(&self) {
        let receiver = self.imp().receiver.borrow_mut().take().unwrap();
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

    fn process_action(&self, action: ApplicationAction) {
        match action {
            ApplicationAction::Present(token) => {
                self.present_main_window(true, token.as_deref());
            }
            ApplicationAction::ToggleWindow(token) => {
                // Playback goes on, as the tray is only shown while Dismas
                // plays in the background
                if let Some(window) = self.active_window().and_downcast::<Window>()
                    && closes_on_click(&window)
                {
                    window.close();
                } else {
                    self.present_main_window(true, token.as_deref());
                }
            }
            ApplicationAction::Quit => self.quit(),
        }
    }

    // With `restore`, a new window restores the playlist of the last
    // session, if the user wants it; otherwise, the caller adds tracks
    fn present_main_window(&self, restore: bool, activation_token: Option<&str>) -> Window {
        let window = if let Some(window) = self.active_window().and_downcast::<Window>() {
            // A window on another desktop comes to this one, rather than
            // taking the user there
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            if window.is_visible()
                && let Some(surface) = window.surface()
                && on_other_desktop(&surface)
                && let Some(surface) = surface.downcast_ref::<gdk_x11::X11Surface>()
            {
                surface.move_to_current_desktop();
            }
            window
        } else {
            let window = Window::new(self);
            if restore {
                window.restore_playlist_on_startup();
            }
            window
        };

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        self.request_background();

        if let Some(token) = activation_token {
            window.set_startup_id(token);
        }
        window.present();
        window
    }

    fn setup_gactions(&self) {
        self.add_action_entries([
            gio::ActionEntry::builder("quit")
                .activate(|app: &Application, _, _| {
                    app.quit();
                })
                .build(),
            gio::ActionEntry::builder("about")
                .activate(|app: &Application, _, _| {
                    app.show_about();
                })
                .build(),
        ]);
    }

    // Without a window, e.g. when activated over D-Bus while playing in the
    // background, the dialog gets a window of its own
    fn show_about(&self) {
        let window = self.active_window();
        let dialog = adw::AboutDialog::builder()
            .application_icon(APPLICATION_ID)
            .application_name("Dismas")
            .developer_name("John Hamilton")
            .version(VERSION)
            .developers(vec!["John Hamilton", "Emmanuele Bassi"])
            .copyright("© 2022 Emmanuele Bassi\n© 2026 John Hamilton")
            .comments(i18n("A fork of Amberol, by Emmanuele Bassi"))
            .website("https://github.com/dachshund7/dismas")
            .issue_url("https://github.com/dachshund7/dismas/issues/new")
            .license_type(gtk::License::Gpl30)
            // Translators: Replace "translator-credits" with your names, one name per line
            .translator_credits(i18n("translator-credits"))
            .build();

        dialog.present(window.as_ref());
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    async fn portal_request_background(&self) {
        if let Some(window) = self.active_window() {
            let root = window.native().unwrap();
            let identifier = WindowIdentifier::from_native(&root).await;
            let request = Background::request().identifier(identifier).reason(&*i18n(
                "Dismas needs to run in the background to play music",
            ));

            // Many sessions have no Background portal, which only asks the
            // user; only a refusal turns the setting off
            match request.send().await.and_then(
                |r: ashpd::desktop::Request<ashpd::desktop::background::Background>| r.response(),
            ) {
                Ok(response) if response.run_in_background() => {
                    debug!("Background request successful: {response:?}");
                }
                Ok(_) | Err(ashpd::Error::Response(_)) => {
                    warn!("Background request denied");
                    // Drops the hold
                    self.imp()
                        .settings
                        .set_boolean("background-play", false)
                        .expect("Unable to set background-play settings key");
                }
                Err(err) => debug!("No Background portal: {err}"),
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn request_background(&self) {
        let imp = self.imp();
        if imp.settings.boolean("background-play") {
            // Held before the portal answers, which can take a while, so
            // that closing the window doesn't quit in the meantime
            if imp.background_hold.borrow().is_none() {
                imp.background_hold.replace(Some(self.hold()));
            }
            let ctx = glib::MainContext::default();
            ctx.spawn_local(clone!(
                #[weak(rename_to = app)]
                self,
                async move { app.portal_request_background().await }
            ));
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    fn request_background(&self) {}
}
