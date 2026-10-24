// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod album_cell;
mod album_tracks;
mod application;
mod audio;
mod config;
mod cover_backdrop;
mod cover_image;
mod cover_picture;
mod cue_sheet;
mod drag_overlay;
mod fit_bin;
mod i18n;
mod library;
mod library_view;
mod marquee;
mod migrate;
mod page_switcher;
mod playback_control;
mod player_box;
mod playlist_view;
mod playlists;
mod playlists_page;
mod preferences_dialog;
mod push_bin;
mod queue_row;
mod resume_point;
mod search;
mod slide_bin;
mod square_bin;
mod track_cover;
mod track_details;
mod track_info_dialog;
mod utils;
mod volume_control;
mod waveform_view;
mod window;
mod work_sections;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod x11_stacking;

use std::env;

use config::{APPLICATION_ID, GETTEXT_PACKAGE, LOCALEDIR, PKGDATADIR, PROFILE};
use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};
use gtk::{gio, glib, prelude::*};
use log::{LevelFilter, debug, error};

use self::application::Application;

fn main() -> glib::ExitCode {
    let mut builder = pretty_env_logger::formatted_builder();
    if APPLICATION_ID.ends_with("Devel") {
        builder.filter(Some("dismas"), LevelFilter::Debug);
    } else {
        builder.filter(Some("dismas"), LevelFilter::Info);
    }
    builder.init();

    // Set up gettext translations
    debug!("Setting up locale data");
    // SAFETY: no other thread is running yet to read the locale
    unsafe {
        setlocale(LocaleCategory::LcAll, "");
    }

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
        env::set_var("PULSE_PROP_application.name", "Dismas");
        env::set_var("PULSE_PROP_media.role", "music");
    }

    debug!("Loading resources");
    let resources = match env::var("MESON_DEVENV") {
        Err(_) => gio::Resource::load(PKGDATADIR.to_owned() + "/dismas.gresource")
            .expect("Unable to find dismas.gresource"),
        Ok(_) => match env::current_exe() {
            Ok(path) => {
                let mut resource_path = path;
                resource_path.pop();
                resource_path.push("dismas.gresource");
                gio::Resource::load(&resource_path)
                    .expect("Unable to find dismas.gresource in devenv")
            }
            Err(err) => {
                error!("Unable to find the current path: {err}");
                return glib::ExitCode::FAILURE;
            }
        },
    };
    gio::resources_register(&resources);

    debug!("Setting up application (profile: {PROFILE})");
    glib::set_application_name("Dismas");
    glib::set_program_name(Some("dismas"));

    gst::init().expect("Failed to initialize gstreamer");

    migrate::migrate_from_amberol();

    Application::new().run()
}
