// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod album_tracks;
mod application;
mod audio;
mod config;
mod cover_image;
mod cover_picture;
mod drag_overlay;
mod i18n;
mod library;
mod library_view;
mod marquee;
mod page_switcher;
mod playback_control;
mod playlist_view;
mod playlists;
mod playlists_page;
mod queue_row;
mod search;
mod track_cover;
mod track_details;
mod track_info_dialog;
mod utils;
mod volume_control;
mod waveform_view;
mod window;
mod work_sections;

use std::env;

use config::{APPLICATION_ID, GETTEXT_PACKAGE, LOCALEDIR, PKGDATADIR, PROFILE};
use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};
use gtk::{gio, glib, prelude::*};
use log::{LevelFilter, debug, error};

use self::application::Application;

fn main() -> glib::ExitCode {
    let mut builder = pretty_env_logger::formatted_builder();
    if APPLICATION_ID.ends_with("Devel") {
        builder.filter(Some("amberol"), LevelFilter::Debug);
    } else {
        builder.filter(Some("amberol"), LevelFilter::Info);
    }
    builder.init();

    // Set up gettext translations
    debug!("Setting up locale data");
    setlocale(LocaleCategory::LcAll, "");

    bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR).expect("Unable to bind the text domain");
    bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8")
        .expect("Unable to set the text domain encoding");
    textdomain(GETTEXT_PACKAGE).expect("Unable to switch to the text domain");

    debug!("Setting up pulseaudio environment");
    let app_id = APPLICATION_ID.trim_end_matches(".Devel");
    // SAFETY: we are still single-threaded at this point, as nothing that
    // can spawn threads (GStreamer, GIO, GTK) has been initialized yet
    unsafe {
        env::set_var("PULSE_PROP_application.icon_name", app_id);
        env::set_var("PULSE_PROP_application.metadata().name", "Amberol");
        env::set_var("PULSE_PROP_media.role", "music");
    }

    debug!("Loading resources");
    let resources = match env::var("MESON_DEVENV") {
        Err(_) => gio::Resource::load(PKGDATADIR.to_owned() + "/amberol.gresource")
            .expect("Unable to find amberol.gresource"),
        Ok(_) => match env::current_exe() {
            Ok(path) => {
                let mut resource_path = path;
                resource_path.pop();
                resource_path.push("amberol.gresource");
                gio::Resource::load(&resource_path)
                    .expect("Unable to find amberol.gresource in devenv")
            }
            Err(err) => {
                error!("Unable to find the current path: {err}");
                return glib::ExitCode::FAILURE;
            }
        },
    };
    gio::resources_register(&resources);

    debug!("Setting up application (profile: {PROFILE})");
    glib::set_application_name("Amberol");
    glib::set_program_name(Some("amberol"));

    gst::init().expect("Failed to initialize gstreamer");

    let ctx = glib::MainContext::default();
    let _guard = ctx.acquire().unwrap();

    Application::new().run()
}
