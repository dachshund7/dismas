// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn covers_dir() -> PathBuf {
    let mut dir = glib::user_cache_dir();
    dir.push("dismas");
    dir.push("album-covers");
    dir
}

// Creates the thumbnails of the albums that don't have one yet, or whose
// tracks changed; the thumbnails are in the cache directory, so they might
// have been removed since the last scan. They're made from the tracks that
// are there, so the albums of the `hidden` folders, which aren't, keep
// theirs until they're back. Returns the number of thumbnails made: one
// made again can keep its name, which the grid can't tell otherwise. An
// album whose cover can't be decoded is tried at every scan, and counts
// for nothing, as its thumbnail stays as it was, missing
pub(super) fn update_covers(
    conn: &mut Connection,
    stale_albums: &HashSet<String>,
    hidden: &[PathBuf],
    sizes: ThumbnailSizes,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<usize> {
    let dir = covers_dir();
    // Each album with its first track, and the thumbnail to make smaller,
    // with its new size, when it's only too large
    let mut missing = Vec::new();
    let mut cover_times = HashMap::new();
    {
        let (hidden_sql, hidden_params) = hidden_condition("t.file", hidden);
        let mut stmt = conn.prepare(&format!(
            "SELECT t.album_id, MIN(t.path), c.album_id, c.path
             FROM tracks t LEFT JOIN covers c ON c.album_id = t.album_id
             WHERE t.album_id IS NOT NULL{hidden_sql}
             GROUP BY t.album_id"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(hidden_params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                path_from_bytes(row.get::<_, Vec<u8>>(1)?),
                row.get::<_, Option<String>>(2)?.is_some(),
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, first_track, has_row, cover) = row?;
            // Thumbnails at another size than the grid shows them, or in
            // the format of older versions, are made again, or smaller
            let update = cover.as_deref().map_or(ThumbnailUpdate::Keep, |p| {
                sizes.update(thumbnail_size(Path::new(p), &dir, &id))
            });
            // Albums without cover art have no thumbnail, so we only check
            // that the thumbnails we created are still there
            let made = cover
                .as_ref()
                .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            let removed = matches!(made, Some(None));
            // A cover file next to the tracks can change without them: it's
            // newer than the thumbnail, or the album had no cover before. The
            // folder is read once, as albums can share one
            let mut cover_time = || {
                let dir = cue_sheet::real_path(&first_track).parent()?;
                *cover_times
                    .entry(dir.to_path_buf())
                    .or_insert_with(|| cover_file_time(dir))
            };
            let cover_file_changed = match made {
                Some(Some(made)) => cover_time().is_some_and(|t| t > made),
                Some(None) => false,
                None => cover_time().is_some(),
            };
            if !has_row
                || update == ThumbnailUpdate::Remake
                || removed
                || cover_file_changed
                || stale_albums.contains(&id)
            {
                // A track that's gone since the scan would leave its album
                // without a cover for good
                if cue_sheet::real_path(&first_track).is_file() {
                    missing.push((id, first_track, None));
                }
            } else if let (ThumbnailUpdate::Shrink(size), Some(cover)) = (update, cover) {
                missing.push((id, first_track, Some((PathBuf::from(cover), size))));
            }
        }
    }

    debug!("Library: generating {} album covers", missing.len());
    let mut n_made = 0;

    glib::mkdir_with_parents(&dir, 0o700);

    let tx = conn.transaction()?;
    {
        let mut upsert =
            tx.prepare("INSERT OR REPLACE INTO covers (album_id, path) VALUES (?1, ?2)")?;
        let total = missing.len();
        if total > 0 {
            let _ = sender.try_send(ScanEvent::Covers(0, total));
        }
        // Each cover is decoded in a sandboxed process, so making all of
        // them again, as when the grid shows them larger, takes a while: a
        // few are made at a time, which goes faster, as for the loader.
        // Making them smaller takes a fraction of that. They're written in
        // this thread, which has the transaction
        let make = |(id, first_track, larger): &(String, PathBuf, Option<(PathBuf, u32)>)| {
            utils::guarded(
                first_track.display(),
                || match larger {
                    Some((thumbnail, size)) => shrink_thumbnail(thumbnail, &dir, id, *size)
                        .or_else(|| create_thumbnail(first_track, &dir, id, sizes.wanted)),
                    None => create_thumbnail(first_track, &dir, id, sizes.wanted),
                },
                || None,
            )
        };
        let mut done = 0;
        utils::in_threads(&missing, utils::READING_THREADS, make, |i, made| {
            let (id, _, larger) = &missing[i];
            done += 1;
            let _ = sender.try_send(ScanEvent::Covers(done, total));
            n_made += usize::from(made.is_some());
            // A thumbnail that couldn't be made smaller is kept as it is
            let cover = made
                .as_deref()
                .or_else(|| larger.as_ref().map(|(thumbnail, _)| thumbnail.as_path()))
                .map(|path| path.to_string_lossy().to_string());
            upsert.execute(params![id, cover])?;
            Ok::<_, rusqlite::Error>(())
        })?;
        tx.execute(
            "DELETE FROM covers WHERE album_id NOT IN
             (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)",
            [],
        )?;
    }
    tx.commit()?;

    remove_unused_covers(conn, &dir)?;
    Ok(n_made)
}

// The sizes the album grid's thumbnails are made at, in pixels
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThumbnailSizes {
    // As large as the grid shows the covers, on the monitor with the
    // largest scale
    pub(crate) wanted: u32,
    // As large as the grid shows them at a scale of 2, the largest the
    // thumbnails are made for
    pub(crate) scale_2: u32,
}

// What a scan does with an album's thumbnail
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThumbnailUpdate {
    Keep,
    // Made smaller, at this size, from the thumbnail itself
    Shrink(u32),
    // Made again from the album's cover
    Remake,
}

impl ThumbnailSizes {
    // What to do with a thumbnail made at `made` pixels, or in the format
    // of older versions (`None`), which is made again, as is one smaller
    // than wanted. One made for a monitor at a scale of 2 is kept, as the
    // monitors can change from one start to the next, and docking a laptop
    // would otherwise make every thumbnail again. Another one larger than
    // wanted was made while the covers were zoomed in: reading a cover
    // smaller than its thumbnail scales it down each time, which takes
    // several times as long, so it's made smaller instead
    pub(crate) fn update(self, made: Option<u32>) -> ThumbnailUpdate {
        match made {
            Some(made) if made == self.wanted || made == self.scale_2 => ThumbnailUpdate::Keep,
            Some(made) if made > self.scale_2.max(self.wanted) => {
                ThumbnailUpdate::Shrink(self.scale_2.max(self.wanted))
            }
            Some(made) if made > self.wanted => ThumbnailUpdate::Shrink(self.wanted),
            _ => ThumbnailUpdate::Remake,
        }
    }
}

// The name of the thumbnail of an album: a JPEG, or a PNG for a cover with
// transparency, which JPEG can't keep. Thumbnails used to be PNG files
// named like the JPEGs, which these names tell apart from the PNG files made
// for transparency, so that the old ones are made again as JPEG
pub(super) fn thumbnail_name(album_id: &str, size: u32, transparent: bool) -> String {
    if transparent {
        format!("{album_id}-{size}-alpha.png")
    } else {
        format!("{album_id}-{size}.jpg")
    }
}

// The size of the album's thumbnail, in either format, if the file is one
// that `thumbnail_name()` names
pub(super) fn thumbnail_size(path: &Path, dir: &Path, album_id: &str) -> Option<u32> {
    if path.parent()? != dir {
        return None;
    }
    let size = path
        .file_name()?
        .to_str()?
        .strip_prefix(album_id)?
        .strip_prefix('-')
        .and_then(|rest| {
            rest.strip_suffix(".jpg")
                .or_else(|| rest.strip_suffix("-alpha.png"))
        })?;
    if size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    size.parse().ok()
}

// Removes the thumbnails of the albums that are gone, or whose id changed,
// and those that were made again at another size or in another format
fn remove_unused_covers(conn: &Connection, dir: &Path) -> rusqlite::Result<()> {
    let used = conn
        .prepare("SELECT path FROM covers WHERE path IS NOT NULL")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()?;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext == "png" || ext == "jpg")
            && !used.contains(path.to_string_lossy().as_ref())
        {
            debug!("Removing unused album cover {path:?}");
            if let Err(e) = std::fs::remove_file(&path) {
                warn!("Unable to remove album cover {path:?}: {e}");
            }
        }
    }

    Ok(())
}

// When the newest cover file for a folder of tracks was changed, if it
// has any; the thumbnail is made from the first track of the album, which
// takes its cover from these files when its tags have none
fn cover_file_time(dir: &Path) -> Option<std::time::SystemTime> {
    CoverCache::cover_files_for(dir)
        .filter_map(|file| std::fs::metadata(file).ok()?.modified().ok())
        .max()
}

// Makes the thumbnail of an album in `dir`, from the cover of its first
// track, and returns its path
fn create_thumbnail(track: &Path, dir: &Path, album_id: &str, size: u32) -> Option<PathBuf> {
    let track = cue_sheet::audio_path(track)?;
    let track = track.as_path();
    let tags = read_file_tags(track, ReadOptions::ALL)?;
    // A file without tags can still have a cover file next to it
    let image = CoverCache::load_cover(&tags.tag, track.parent(), size)?;
    save_thumbnail(&image, dir, album_id, size)
}

// Makes a smaller thumbnail of an album from a larger one, made while the
// covers were zoomed in, and returns its path. The larger one is read in the
// process (`CoverImage::open_thumbnail`), which takes a fraction of the time
// that decoding the cover again in the sandbox would
pub(super) fn shrink_thumbnail(
    thumbnail: &Path,
    dir: &Path,
    album_id: &str,
    size: u32,
) -> Option<PathBuf> {
    let image = CoverImage::open_thumbnail(thumbnail)?.thumbnail(size)?;
    save_thumbnail(&image, dir, album_id, size)
}

fn save_thumbnail(image: &CoverImage, dir: &Path, album_id: &str, size: u32) -> Option<PathBuf> {
    let dest = dir.join(thumbnail_name(album_id, size, image.has_alpha()));
    let saved = if image.has_alpha() {
        image.save_png(&dest)
    } else {
        image.save_jpeg(&dest)
    };

    match saved {
        Ok(_) => Some(dest),
        Err(e) => {
            warn!("Unable to save album cover {dest:?}: {e}");
            None
        }
    }
}
