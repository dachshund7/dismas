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
mod play_stats;
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

use std::{
    env,
    path::{Path, PathBuf},
};

use config::{APPLICATION_ID, GETTEXT_PACKAGE, LOCALEDIR, PKGDATADIR, PROFILE};
use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};
use gtk::{gio, glib, prelude::*};
use log::{LevelFilter, debug, error};

use self::application::Application;

// Where the data installed along with the binary is
#[derive(Debug, PartialEq)]
struct DataPaths {
    resources: PathBuf,
    locale: PathBuf,
    // The settings schema compiled in a folder of its own, which only the
    // prebuilt tarball has; a Meson install compiles it with the others
    schemas: Option<PathBuf>,
}

// The data installed next to the binary, as `<prefix>/share/dismas` is to
// `<prefix>/bin/dismas`, so that a build unpacked anywhere, like the
// prebuilt tarball, finds its own; or else where Meson installed it, which
// for a Meson install is the same place. The data next to the binary comes
// first, as another install at the prefix the build was made for could have
// other versions of it
fn data_paths(exe: Option<&Path>, exists: impl Fn(&Path) -> bool) -> DataPaths {
    if let Some(prefix) = exe.and_then(Path::parent).and_then(Path::parent) {
        let share = prefix.join("share");
        let resources = share.join("dismas").join("dismas.gresource");
        if exists(&resources) {
            let locale = share.join("locale");
            let schemas = share.join("dismas").join("schemas");
            return DataPaths {
                resources,
                locale: if exists(&locale) {
                    locale
                } else {
                    PathBuf::from(LOCALEDIR)
                },
                schemas: exists(&schemas.join("gschemas.compiled")).then_some(schemas),
            };
        }
    }
    DataPaths {
        resources: Path::new(PKGDATADIR).join("dismas.gresource"),
        locale: PathBuf::from(LOCALEDIR),
        schemas: None,
    }
}

fn main() -> glib::ExitCode {
    // RUST_LOG, as in RUST_LOG=dismas=debug, chooses what's logged, so a
    // release build, like the Flatpak's, can give a debug log too; without
    // it, development builds log debug messages and release builds from
    // info up. The default isn't added to RUST_LOG, as its setting for
    // dismas would take precedence over a general one like RUST_LOG=debug
    let mut builder = pretty_env_logger::formatted_builder();
    match env::var("RUST_LOG") {
        Ok(filters) if !filters.is_empty() => {
            builder.parse_filters(&filters);
        }
        _ if APPLICATION_ID.ends_with("Devel") => {
            builder.filter(Some("dismas"), LevelFilter::Debug);
        }
        _ => {
            builder.filter(Some("dismas"), LevelFilter::Info);
        }
    }
    builder.init();

    // The path of the binary with its symbolic links resolved, so a link to
    // it, like the one the tarball's install.sh makes, finds the data too
    let paths = data_paths(env::current_exe().ok().as_deref(), Path::exists);
    debug!("Data: {paths:?}");

    // Set up gettext translations
    debug!("Setting up locale data");
    // SAFETY: no other thread is running yet to read the locale
    unsafe {
        setlocale(LocaleCategory::LcAll, "");
    }

    bindtextdomain(GETTEXT_PACKAGE, &paths.locale).expect("Unable to bind the text domain");
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

    // GLib looks for schemas in GSETTINGS_SCHEMA_DIR before the XDG data
    // folders, so the tarball's own schema is found without being installed
    // anywhere, and it comes before the schema of another install
    if let Some(schemas) = &paths.schemas {
        let others = env::var_os("GSETTINGS_SCHEMA_DIR").unwrap_or_default();
        match env::join_paths(std::iter::once(schemas.clone()).chain(env::split_paths(&others))) {
            // SAFETY: still single-threaded, as above
            Ok(dirs) => unsafe { env::set_var("GSETTINGS_SCHEMA_DIR", dirs) },
            Err(err) => error!("Unable to add the schemas in {schemas:?}: {err}"),
        }
    }

    debug!("Loading resources");
    let resources = match env::var("MESON_DEVENV") {
        Err(_) => gio::Resource::load(&paths.resources).expect("Unable to find dismas.gresource"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exists_in(files: &[&str]) -> impl Fn(&Path) -> bool {
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        move |path| files.iter().any(|file| file == path)
    }

    #[test]
    fn data_of_the_tarball_next_to_the_binary() {
        let exists = exists_in(&[
            "/opt/dismas-2026.11/share/dismas/dismas.gresource",
            "/opt/dismas-2026.11/share/dismas/schemas/gschemas.compiled",
            "/opt/dismas-2026.11/share/locale",
        ]);
        let paths = data_paths(Some(Path::new("/opt/dismas-2026.11/bin/dismas")), exists);
        assert_eq!(
            paths,
            DataPaths {
                resources: PathBuf::from("/opt/dismas-2026.11/share/dismas/dismas.gresource"),
                locale: PathBuf::from("/opt/dismas-2026.11/share/locale"),
                schemas: Some(PathBuf::from("/opt/dismas-2026.11/share/dismas/schemas")),
            }
        );
    }

    #[test]
    fn data_next_to_the_binary_without_schemas_of_its_own() {
        // As a Meson install is laid out, its schema compiled with the others
        let exists = exists_in(&[
            "/home/me/.local/share/dismas/dismas.gresource",
            "/home/me/.local/share/locale",
        ]);
        let paths = data_paths(Some(Path::new("/home/me/.local/bin/dismas")), exists);
        assert_eq!(
            paths.resources,
            PathBuf::from("/home/me/.local/share/dismas/dismas.gresource")
        );
        assert_eq!(paths.locale, PathBuf::from("/home/me/.local/share/locale"));
        assert_eq!(paths.schemas, None);
    }

    #[test]
    fn data_where_meson_installed_it_without_any_next_to_the_binary() {
        let paths = data_paths(Some(Path::new("/somewhere/bin/dismas")), exists_in(&[]));
        assert_eq!(
            paths,
            DataPaths {
                resources: Path::new(PKGDATADIR).join("dismas.gresource"),
                locale: PathBuf::from(LOCALEDIR),
                schemas: None,
            }
        );
        let paths = data_paths(None, exists_in(&[]));
        assert_eq!(
            paths.resources,
            Path::new(PKGDATADIR).join("dismas.gresource")
        );
    }
}
