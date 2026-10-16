// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use core::cmp::Ordering;

use gtk::{gio, glib, prelude::*};
use log::{debug, warn};

use crate::{
    audio::{Queue, Track},
    config::APPLICATION_ID,
};

// The path of a file outside the sandbox, for the files that came through
// the document portal
pub const ATTRIBUTE_HOST_PATH: &str = "xattr::document-portal.host-path";

// Selects the tracks of a list from `from` to `to`, both included, in the
// order of the list; returns false if either is not in it
pub fn select_track_range(model: &gio::ListModel, from: &Track, to: &Track) -> bool {
    let position = |track: &Track| {
        (0..model.n_items()).find(|&i| model.item(i).and_downcast_ref::<Track>() == Some(track))
    };
    let (Some(from), Some(to)) = (position(from), position(to)) else {
        return false;
    };

    for i in from.min(to)..=from.max(to) {
        if let Some(track) = model.item(i).and_downcast::<Track>() {
            track.set_selected(true);
        }
    }
    true
}

pub fn settings_manager() -> gio::Settings {
    // We ship a single schema for both default and development profiles
    let app_id = APPLICATION_ID.trim_end_matches(".Devel");
    gio::Settings::new(app_id)
}

pub fn format_remaining_time(t: i64) -> String {
    // We use an explicit LRM character so the MINUS SIGN character
    // stays in front of the remaining time even in RTL locales, instead
    // of getting flipped at the end of the text
    format!("\u{200e}\u{2212}{}:{:02}", (t - (t % 60)) / 60, t % 60)
}

pub fn format_time(t: i64) -> String {
    format!("{}:{:02}", (t - (t % 60)) / 60, t % 60)
}

// Whether a file is an audio track, guessing from its name only, which is
// much faster than looking at its contents when scanning a whole library
pub fn is_audio_path(path: &std::path::Path) -> bool {
    let (mime_type, _) = gio::content_type_guess(Some(path), None);
    mime_type.starts_with("audio") && !mime_type.ends_with("x-mpegurl")
}

fn load_files_from_folder_internal(
    base: &gio::File,
    folder: &gio::File,
    recursive: bool,
) -> Vec<gio::File> {
    let Ok(mut enumerator) = folder.enumerate_children(
        "standard::name,standard::type",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        None::<&gio::Cancellable>,
    ) else {
        warn!("Unable to enumerate {}", folder.uri());
        return Vec::new();
    };

    let mut files = Vec::new();
    while let Some(info) = enumerator.next().and_then(Result::ok) {
        let child = enumerator.child(&info);
        if recursive && info.file_type() == gio::FileType::Directory {
            let mut res = load_files_from_folder_internal(base, &child, recursive);
            files.append(&mut res);
        } else if info.file_type() == gio::FileType::Regular {
            files.push(child.clone());
        }
    }

    // gio::FileEnumerator has no guaranteed order, so we should
    // rely on the basename being formatted in a way that gives us an
    // implicit order; if anything, this will queue tracks in the same
    // order in which they appear in the directory when browsing its
    // contents
    files.sort_by(|a, b| cmp_two_files(Some(base), a, b));

    files
}

pub fn cmp_two_files(base: Option<&gio::File>, a: &gio::File, b: &gio::File) -> Ordering {
    cmp_like_nautilus(&folder_sort_name(base, a), &folder_sort_name(base, b))
        .then_with(|| cmp_like_nautilus(&file_sort_name(a), &file_sort_name(b)))
}

// The folder of the file, relative to the base folder if it's inside it,
// or else by its own name
fn folder_sort_name(base: Option<&gio::File>, file: &gio::File) -> String {
    let Some(parent) = file.parent() else {
        return String::new();
    };
    base.and_then(|base| base.relative_path(&parent))
        .or_else(|| parent.basename())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// Files that aren't paths, like some GVFS locations, have no basename
fn file_sort_name(file: &gio::File) -> String {
    file.basename().map_or_else(
        || file.uri().to_string(),
        |path| path.to_string_lossy().into_owned(),
    )
}

fn cmp_like_nautilus(filename_a: &str, filename_b: &str) -> Ordering {
    let sort_last_a = filename_a.starts_with(['.', '#']);
    let sort_last_b = filename_b.starts_with(['.', '#']);

    if !sort_last_a && sort_last_b {
        Ordering::Less
    } else if sort_last_a && !sort_last_b {
        Ordering::Greater
    } else {
        let key_a = glib::FilenameCollationKey::from(filename_a);
        let key_b = glib::FilenameCollationKey::from(filename_b);
        key_a.cmp(&key_b)
    }
}

pub fn load_files_from_folder(folder: &gio::File, recursive: bool) -> Vec<gio::File> {
    use std::time::Instant;

    let now = Instant::now();
    let res = load_files_from_folder_internal(folder, folder, recursive);
    debug!(
        "Folder enumeration: {} us (recursive: {}), total files: {}",
        now.elapsed().as_micros(),
        recursive,
        res.len()
    );

    res
}

pub fn store_playlist(queue: &Queue) {
    let pls = glib::KeyFile::new();
    pls.set_string("playlist", "X-GNOME-Title", "Amberol's current playlist");

    pls.set_int64("playlist", "NumberOfEntries", i64::from(queue.n_tracks()));

    // URIs, unlike paths, are valid UTF-8, and have no line breaks
    for (i, track) in queue.model().iter::<Track>().flatten().enumerate() {
        pls.set_value("playlist", &format!("File{i}"), &track.uri());
    }

    let mut pls_cache = glib::user_cache_dir();
    pls_cache.push("amberol");
    pls_cache.push("playlists");
    glib::mkdir_with_parents(&pls_cache, 0o755);

    pls_cache.push("current.pls");
    match pls.save_to_file(&pls_cache) {
        Ok(_) => debug!("Current playlist updated to: {pls_cache:?}"),
        Err(e) => debug!("Unable to save current playlist: {e}"),
    }
}

pub fn load_cached_tracks() -> Option<Vec<gio::File>> {
    let mut pls_cache = glib::user_cache_dir();
    pls_cache.push("amberol");
    pls_cache.push("playlists");
    pls_cache.push("current.pls");

    let pls = glib::KeyFile::new();
    if let Err(e) = pls.load_from_file(&pls_cache, glib::KeyFileFlags::NONE) {
        debug!("Unable to load current playlist: {e}");
        return None;
    }

    // The entries are read from the keys, rather than up to the number of
    // entries, which a damaged file could get wrong
    let keys = match pls.keys("playlist") {
        Ok(keys) => keys,
        Err(e) => {
            debug!("Unable to read current playlist: {e}");
            return None;
        }
    };
    let mut entries: Vec<(u64, gio::File)> = keys
        .iter()
        .filter_map(|key| {
            let index = key.as_str().strip_prefix("File")?.parse().ok()?;
            let value = pls.value("playlist", key.as_str()).ok()?;
            // Older versions saved paths
            let file = if value.starts_with('/') {
                gio::File::for_path(value.as_str())
            } else {
                gio::File::for_uri(value.as_str())
            };
            Some((index, file))
        })
        .collect();
    entries.sort_by_key(|(index, _)| *index);

    Some(entries.into_iter().map(|(_, file)| file).collect())
}

pub fn has_cached_playlist() -> bool {
    let mut pls_cache = glib::user_cache_dir();
    pls_cache.push("amberol");
    pls_cache.push("playlists");
    pls_cache.push("current.pls");

    pls_cache.exists()
}
