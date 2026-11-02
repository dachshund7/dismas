// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod album_cell;
mod album_tracks;
mod application;
mod audio;
mod booklets;
mod catalogue;
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
mod lyrics;
mod lyrics_view;
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
mod reveal_bin;
mod search;
mod search_query;
mod search_syntax;
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
mod zoom;
mod zoom_row;

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

// The folders that GLib looks for schemas in, for GSETTINGS_SCHEMA_DIR: the
// tarball's `schemas`, then those it already had. Not an empty one, which
// splitting an empty variable gives, as GLib takes it for the current
// folder, and would trust the schemas there over those of the system
fn schema_dirs(
    schemas: &Path,
    others: &std::ffi::OsStr,
) -> Result<std::ffi::OsString, env::JoinPathsError> {
    let others = env::split_paths(others).filter(|dir| !dir.as_os_str().is_empty());
    env::join_paths(std::iter::once(schemas.to_path_buf()).chain(others))
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

    // Dismas started to read a file with GStreamer, for the Dismas that
    // started it, which mustn't make an application: GApplication would
    // pass the arguments on to the Dismas running
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args
        .first()
        .is_some_and(|arg| arg == audio::discovered::CHILD_ARG)
    {
        return audio::discovered::child_main(&args[1..]);
    }
    // The child is Dismas itself. On Linux, it's the binary running, even
    // if another was installed over it since, so that the two agree
    #[cfg(target_os = "linux")]
    audio::discovered::set_helper(PathBuf::from("/proc/self/exe"));
    #[cfg(not(target_os = "linux"))]
    if let Ok(exe) = env::current_exe() {
        audio::discovered::set_helper(exe);
    }

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
        match schema_dirs(schemas, &others) {
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

    audio::init_gstreamer().expect("Failed to initialize gstreamer");

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

    // GLib would trust a gschemas.compiled in the current folder for an
    // empty entry, which an unset variable gave
    #[test]
    fn the_schema_folders_have_no_empty_entry() {
        let dirs = |others: &str| {
            schema_dirs(Path::new("/opt/d/schemas"), std::ffi::OsStr::new(others)).unwrap()
        };
        assert_eq!(dirs(""), "/opt/d/schemas");
        assert_eq!(dirs("/a:/b"), "/opt/d/schemas:/a:/b");
        assert_eq!(dirs(":/a::/b:"), "/opt/d/schemas:/a:/b");
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
