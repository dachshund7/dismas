// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

pub(super) fn covers_dir() -> PathBuf {
    let mut dir = glib::user_cache_dir();
    dir.push("dismas");
    dir.push("album-covers");
    dir
}

// What `update_covers` made
#[derive(Debug, Default)]
pub(super) struct MadeCovers {
    // The number of thumbnails made: one made again can keep its name,
    // which the grid can't tell otherwise
    pub(super) count: usize,
    // The tracks of the albums whose cover files changed without them, or
    // that are new with cover files, which the queue has to look for
    // their cover again
    pub(super) tracks_with_new_cover: Vec<PathBuf>,
}

// An album whose thumbnail `update_covers` makes
struct CoverJob {
    id: String,
    first_track: PathBuf,
    // The thumbnail to make smaller, with its new size, when it's only
    // too large
    larger: Option<(PathBuf, u32)>,
    // The key of the cover files next to its first track, kept with the
    // thumbnail, "" when there are none
    cover_files: String,
    // Whether they changed without its tracks
    cover_files_changed: bool,
}

// Creates the thumbnails of the albums that don't have one yet, or whose
// tracks or cover files changed, in `dir`, the cache directory, so they
// might have been removed since the last scan. They're made from the
// tracks that are there, so the albums of the `hidden` folders, which
// aren't, keep theirs until they're back. An album whose cover can't be
// decoded is tried at every scan while it has cover files, and counts for
// nothing, as its thumbnail stays as it was, missing
pub(super) fn update_covers(
    conn: &mut Connection,
    dir: &Path,
    stale_albums: &HashSet<String>,
    hidden: &[PathBuf],
    sizes: ThumbnailSizes,
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<MadeCovers> {
    let mut missing = Vec::new();
    // The albums kept as they are whose cover files aren't known yet, as
    // an older version made their thumbnail, with the key to keep
    let mut unknown = Vec::new();
    // The folders are read once, as albums can share one
    let mut folder_keys = HashMap::new();
    {
        let (hidden_sql, hidden_params) = hidden_condition("t.file", hidden);
        let mut stmt = conn.prepare(&format!(
            "SELECT t.album_id, MIN(t.path), c.album_id, c.path, c.cover_files
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
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (id, first_track, has_row, cover, kept_files) = row?;
            // Thumbnails at another size than the grid shows them, or in
            // the format of older versions, are made again, or smaller
            let update = cover.as_deref().map_or(ThumbnailUpdate::Keep, |p| {
                sizes.update(thumbnail_size(Path::new(p), dir, &id))
            });
            // Albums without cover art have no thumbnail, so we only check
            // that the thumbnails we created are still there
            let removed = cover
                .as_deref()
                .is_some_and(|p| std::fs::metadata(p).is_err());
            // A cover file next to the tracks can change without them: it's
            // added, removed or replaced
            let files = cue_sheet::real_path(&first_track)
                .parent()
                .and_then(|folder| {
                    folder_keys
                        .entry(folder.to_path_buf())
                        .or_insert_with(|| cover_files_key(folder))
                        .clone()
                });
            let cover_files = files.clone().unwrap_or_default();
            // The tracks of an album that's new to the library can have
            // reached the playlist before its cover file, as when a download
            // is added to it before its cover is written, so they're listed
            // too, once, as the key is kept
            let cover_files_changed = if has_row {
                kept_files.as_ref().is_some_and(|k| *k != cover_files)
            } else {
                files.is_some()
            };
            // An album without a thumbnail whose cover file couldn't be
            // decoded is tried again, as it was before keys were kept
            let retried = has_row && cover.is_none() && files.is_some();
            if !has_row
                || update == ThumbnailUpdate::Remake
                || removed
                || cover_files_changed
                || retried
                || stale_albums.contains(&id)
            {
                // A track that's gone since the scan would leave its album
                // without a cover for good
                if cue_sheet::real_path(&first_track).is_file() {
                    missing.push(CoverJob {
                        id,
                        first_track,
                        larger: None,
                        cover_files,
                        cover_files_changed,
                    });
                }
            } else if let (ThumbnailUpdate::Shrink(size), Some(cover)) = (update, cover) {
                missing.push(CoverJob {
                    id,
                    first_track,
                    larger: Some((PathBuf::from(cover), size)),
                    cover_files,
                    cover_files_changed: false,
                });
            } else if has_row && kept_files.is_none() {
                unknown.push((id, cover_files));
            }
        }
    }

    debug!("Library: generating {} album covers", missing.len());
    let mut made_covers = MadeCovers::default();
    // The albums whose cover files changed; their key is kept, so they're
    // listed once, even when their cover can't be decoded, as their tracks
    // in the queue can still show the cover that was removed
    let mut new_cover_files = Vec::new();

    glib::mkdir_with_parents(dir, 0o700);

    let tx = conn.transaction()?;
    {
        let mut upsert = tx.prepare(
            "INSERT OR REPLACE INTO covers (album_id, path, cover_files) VALUES (?1, ?2, ?3)",
        )?;
        let total = missing.len();
        if total > 0 {
            let _ = sender.try_send(ScanEvent::Covers(0, total));
        }
        // Each cover is decoded in a sandboxed process, so making all of
        // them again, as when the grid shows them larger, takes a while: a
        // few are made at a time, which goes faster, as for the loader.
        // Making them smaller takes a fraction of that. They're written in
        // this thread, which has the transaction
        let make = |job: &CoverJob| {
            utils::guarded(
                job.first_track.display(),
                || match &job.larger {
                    Some((thumbnail, size)) => shrink_thumbnail(thumbnail, dir, &job.id, *size)
                        .or_else(|| create_thumbnail(&job.first_track, dir, &job.id, sizes.wanted)),
                    None => create_thumbnail(&job.first_track, dir, &job.id, sizes.wanted),
                },
                || None,
            )
        };
        let mut done = 0;
        utils::in_threads(&missing, utils::READING_THREADS, make, |i, made| {
            let job = &missing[i];
            done += 1;
            let _ = sender.try_send(ScanEvent::Covers(done, total));
            made_covers.count += usize::from(made.is_some());
            if job.cover_files_changed {
                new_cover_files.push(job.id.as_str());
            }
            // A thumbnail that couldn't be made smaller is kept as it is
            let cover = made
                .as_deref()
                .or_else(|| {
                    job.larger
                        .as_ref()
                        .map(|(thumbnail, _)| thumbnail.as_path())
                })
                .map(|path| path.to_string_lossy().to_string());
            upsert.execute(params![job.id, cover, job.cover_files])?;
            Ok::<_, rusqlite::Error>(())
        })?;
        let mut keep_key = tx.prepare("UPDATE covers SET cover_files = ?2 WHERE album_id = ?1")?;
        for (id, cover_files) in &unknown {
            keep_key.execute([id, cover_files])?;
        }
        tx.execute(
            "DELETE FROM covers WHERE album_id NOT IN
             (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)",
            [],
        )?;

        // All the tracks of the album, as those in another disc folder than
        // its first track's keep their cover apart, and can show the same
        // cover file, in the folder above
        let mut album_tracks = tx.prepare("SELECT path FROM tracks WHERE album_id = ?1")?;
        for id in new_cover_files {
            let paths = album_tracks.query_map([id], |row| row.get::<_, Vec<u8>>(0))?;
            for path in paths {
                made_covers
                    .tracks_with_new_cover
                    .push(path_from_bytes(path?));
            }
        }
    }
    tx.commit()?;

    remove_unused_covers(conn, dir)?;
    Ok(made_covers)
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

// What the cover files for a folder of tracks are, if it has any: a hash
// of their names, sizes and modification times, which the scan keeps with
// the album's thumbnail; the thumbnail is made from the first track of
// the album, which takes its cover from these files when its tags have
// none. Any change to them changes the key, a file added, removed or
// replaced, even by an older file, as moving or copying one can keep the
// time it was written, and whatever the clock said then: comparing their
// times with the thumbnail's missed a cover moved in from elsewhere, and
// made a cover dated in the future again at every scan. The names are
// those of the files, so that moving the music folder keeps the keys
fn cover_files_key(dir: &Path) -> Option<String> {
    let mut hasher = Sha256::new();
    let mut any = false;
    for file in CoverCache::cover_files_for(dir) {
        let Ok(metadata) = std::fs::metadata(&file) else {
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default();
        hasher.update(file.file_name().unwrap_or_default().as_encoded_bytes());
        hasher.update([0]);
        hasher.update(metadata.len().to_le_bytes());
        hasher.update(modified.as_nanos().to_le_bytes());
        any = true;
    }
    any.then(|| utils::hex(&hasher.finalize()))
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
