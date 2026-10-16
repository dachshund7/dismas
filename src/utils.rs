// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use core::cmp::Ordering;
use std::path::PathBuf;

use color_thief::{ColorFormat, get_palette};
use gtk::{gdk, gio, glib, prelude::*};
use log::{debug, warn};

use crate::{
    audio::{Queue, Track},
    config::APPLICATION_ID,
};

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

// The base cover size is 192px, but we need to account for HiDPI;
// better to scale down when rendering on displays with a scaling
// factor of 1 than having to scale up on displays with a scaling
// factor of 2.
const COVER_SIZE: i32 = 192 * 2;

pub fn load_cover_texture(buffer: &glib::Bytes) -> Option<gdk_pixbuf::Pixbuf> {
    let stream = gio::MemoryInputStream::from_bytes(buffer);

    if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE) {
        let width = pixbuf.width();
        let height = pixbuf.height();
        let ratio = width as f32 / height as f32;

        let w: i32;
        let h: i32;

        if ratio > 1.0 {
            w = COVER_SIZE;
            h = (COVER_SIZE as f32 / ratio) as i32;
        } else {
            w = (COVER_SIZE as f32 * ratio) as i32;
            h = COVER_SIZE;
        }

        debug!("Cover size {width} x {height} (ratio: {ratio}), scaled: {w} x {h}");

        pixbuf.scale_simple(w, h, gdk_pixbuf::InterpType::Bilinear)
    } else {
        warn!("Unable to load cover art");
        None
    }
}

// The cover in the main view is drawn larger than the shared cover
// texture, so it is loaded separately for the current track; we limit its
// size, as the original artwork can be very large, and let the renderer
// scale it down the rest of the way
const FULL_COVER_SIZE: i32 = 1024;

pub fn load_full_cover(buffer: &glib::Bytes) -> Option<gdk_pixbuf::Pixbuf> {
    load_scaled_cover(buffer, FULL_COVER_SIZE)
}

// Loads the cover art, scaled down to fit in `max_size`, keeping its
// aspect ratio; smaller covers are not scaled up
pub fn load_scaled_cover(buffer: &glib::Bytes, max_size: i32) -> Option<gdk_pixbuf::Pixbuf> {
    let stream = gio::MemoryInputStream::from_bytes(buffer);
    let pixbuf = gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE).ok()?;

    let width = pixbuf.width();
    let height = pixbuf.height();
    if width <= max_size && height <= max_size {
        return Some(pixbuf);
    }

    let (w, h) = if width > height {
        (max_size, max_size * height / width)
    } else {
        (max_size * width / height, max_size)
    };

    pixbuf.scale_simple(w, h, gdk_pixbuf::InterpType::Hyper)
}

// Whether a file is an audio track, guessing from its name only, which is
// much faster than looking at its contents when scanning a whole library
pub fn is_audio_path(path: &std::path::Path) -> bool {
    let (mime_type, _) = gio::content_type_guess(Some(path), None);
    mime_type.starts_with("audio") && !mime_type.ends_with("x-mpegurl")
}

pub fn cache_cover_art(uuid: &str, pixbuf: &gdk_pixbuf::Pixbuf) -> Option<PathBuf> {
    let mut cache_dir = glib::user_cache_dir();
    cache_dir.push("amberol");
    cache_dir.push("covers");
    glib::mkdir_with_parents(&cache_dir, 0o755);

    cache_dir.push(format!("{}.png", uuid));
    let file = gio::File::for_path(&cache_dir);
    match file.create(gio::FileCreateFlags::NONE, gio::Cancellable::NONE) {
        Ok(stream) => {
            debug!("Creating cover data cache at {:?}", cache_dir);
            pixbuf.save_to_streamv_async(
                &stream,
                "png",
                &[("tEXt::Software", "amberol")],
                gio::Cancellable::NONE,
                move |res| {
                    if let Err(e) = res {
                        warn!("Unable to cache cover data: {}", e);
                    }
                },
            );
        }
        Err(e) => {
            if let Some(file_error) = e.kind::<glib::FileError>() {
                match file_error {
                    glib::FileError::Exist => (),
                    _ => {
                        warn!("Unable to create cache file: {}", e);
                        return None;
                    }
                };
            }
        }
    };

    Some(cache_dir)
}

fn color_format(has_alpha: bool) -> ColorFormat {
    if has_alpha {
        ColorFormat::Rgba
    } else {
        ColorFormat::Rgb
    }
}

pub fn load_palette(pixbuf: &gdk_pixbuf::Pixbuf) -> Option<Vec<gdk::RGBA>> {
    if let Ok(palette) = get_palette(
        pixbuf.pixel_bytes().unwrap().as_ref(),
        color_format(pixbuf.has_alpha()),
        5,
        4,
    ) {
        let colors: Vec<gdk::RGBA> = palette
            .iter()
            .map(|c| {
                gdk::RGBA::new(
                    c.r as f32 / 255.0,
                    c.g as f32 / 255.0,
                    c.b as f32 / 255.0,
                    1.0,
                )
            })
            .collect();

        return Some(colors);
    }

    None
}

fn load_files_from_folder_internal(
    base: &gio::File,
    folder: &gio::File,
    recursive: bool,
) -> Vec<gio::File> {
    let mut enumerator = if let Ok(enumerator) = folder.enumerate_children(
        "standard::name,standard::type",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        None::<&gio::Cancellable>,
    ) {
        enumerator
    } else {
        warn!("Unable to enumerate {}", folder.uri());
        return Vec::new();
    };

    let mut files = Vec::new();
    while let Some(info) = enumerator.next().and_then(|s| s.ok()) {
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
    file.basename()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.uri().to_string())
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

    pls.set_int64("playlist", "NumberOfEntries", queue.n_tracks() as i64);

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
        Ok(_) => debug!("Current playlist updated to: {:?}", pls_cache),
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
