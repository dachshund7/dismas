// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bringing over the data and settings of Amberol, which Dismas is a fork of

use std::{fs, io, path::Path};

use gtk::{gio, glib, prelude::*};
use log::{info, warn};

use crate::{config::APPLICATION_ID, utils};

const AMBEROL_DIR: &str = "amberol";
const AMBEROL_SCHEMA: &str = "io.bassi.Amberol";
const MIGRATED_KEY: &str = "amberol-migrated";

// The files that Dismas keeps in its data directory; the directory itself
// can exist before the first run, when the app is installed in ~/.local
const DATA_FILES: [&str; 4] = ["library.db", "library.db-wal", "queue.m3u8", "playlists"];

// On the first run, copies the library, the queue and the playlists of
// Amberol, and the settings that were changed in it. Amberol's own copies
// are left alone, and the caches are made again as they're needed. It's
// recorded in the settings, which outlive the data folder: once Dismas has
// settings of its own, Amberol's must not come back over them
pub fn migrate_from_amberol() {
    let settings = utils::settings_manager();
    if settings.boolean(MIGRATED_KEY) {
        return;
    }

    let data_dir = glib::user_data_dir().join("dismas");
    if !DATA_FILES.iter().any(|name| data_dir.join(name).exists()) {
        let amberol_dir = glib::user_data_dir().join(AMBEROL_DIR);
        if amberol_dir.is_dir() {
            info!("Copying the data of Amberol from {}", amberol_dir.display());
            if let Err(err) = copy_data(&amberol_dir, &data_dir) {
                warn!("Unable to copy the data of Amberol: {err}");
            }
        }
        copy_settings();
    }

    if let Err(err) = settings.set_boolean(MIGRATED_KEY, true) {
        warn!("Unable to store {MIGRATED_KEY}: {err}");
    }
    gio::Settings::sync();
}

fn copy_data(from: &Path, to: &Path) -> io::Result<()> {
    make_dir(to)?;
    for name in DATA_FILES {
        let source = from.join(name);
        if source.is_dir() {
            make_dir(&to.join(name))?;
            for entry in fs::read_dir(&source)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    copy_file(&entry.path(), &to.join(name).join(entry.file_name()))?;
                }
            }
        } else if source.is_file() {
            copy_file(&source, &to.join(name))?;
        }
    }
    Ok(())
}

// The user's own, as the XDG spec asks of the folders an app makes
fn make_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

// Copied under a temporary name, and renamed once it's complete, so that an
// interrupted copy doesn't leave a truncated file to be taken for a whole one
fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    let mut partial = to.as_os_str().to_owned();
    partial.push(".partial");
    fs::copy(from, &partial)?;
    fs::rename(&partial, to)
}

// Amberol's schema is only there while Amberol is installed
fn copy_settings() {
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return;
    };
    let Some(old_schema) = source.lookup(AMBEROL_SCHEMA, true) else {
        return;
    };
    let Some(new_schema) = source.lookup(APPLICATION_ID.trim_end_matches(".Devel"), true) else {
        return;
    };

    info!("Copying the settings of Amberol");
    let old = gio::Settings::new_full(&old_schema, None::<&gio::SettingsBackend>, None);
    let new = utils::settings_manager();
    for key in old_schema.list_keys() {
        if !new_schema.has_key(&key) {
            continue;
        }
        // Keys left at their default keep the default of Dismas
        let Some(value) = old.user_value(&key) else {
            continue;
        };
        // A key whose type changed can't be copied, and range_check()
        // only takes values of the key's own type
        let new_key = new_schema.key(&key);
        if value.type_() != new_key.value_type() || !new_key.range_check(&value) {
            continue;
        }
        if let Err(err) = new.set_value(&key, &value) {
            warn!("Unable to copy the setting {key}: {err}");
        }
    }
    gio::Settings::sync();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_only_the_data_files() {
        let from = glib::mkdtemp("dismas-amberol-XXXXXX").unwrap();
        let root = glib::mkdtemp("dismas-data-XXXXXX").unwrap();
        let to = root.join("dismas");
        fs::write(from.join("library.db"), "db").unwrap();
        fs::write(from.join("queue.m3u8"), "#EXTM3U\n").unwrap();
        // Installed in ~/.local, Amberol keeps its resources here too
        fs::write(from.join("amberol.gresource"), "resources").unwrap();
        fs::create_dir(from.join("playlists")).unwrap();
        fs::write(from.join("playlists").join("Carmen.m3u8"), "#EXTM3U\n").unwrap();

        copy_data(&from, &to).unwrap();

        assert_eq!(fs::read_to_string(to.join("library.db")).unwrap(), "db");
        assert!(to.join("queue.m3u8").is_file());
        assert!(to.join("playlists").join("Carmen.m3u8").is_file());
        assert!(!to.join("amberol.gresource").exists());
        // Nothing left under a temporary name
        assert!(!to.join("queue.m3u8.partial").exists());
        // Files that weren't there aren't made up
        assert!(!to.join("library.db-wal").exists());

        fs::remove_dir_all(&from).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }
}
