// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;

use super::*;
use crate::audio::{FolderCovers, folder_covers};

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
    // The tracks of the albums whose cover files changed without them,
    // see `Listed::All`
    pub(super) tracks_with_new_cover: Vec<PathBuf>,
    // The tracks of the albums new to the library with cover files, see
    // `Listed::WithoutCover`
    pub(super) tracks_of_new_albums: Vec<PathBuf>,
}

// Which tracks of an album the scan lists, for the queue to look for
// their cover again; an album is listed once, as its key is kept, even
// when its cover can't be decoded
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listed {
    No,
    // Its cover files changed without its tracks: all of them, as they
    // can show the cover that was replaced or removed
    All,
    // It's new to the library, with cover files: those the queue has
    // without a cover, which reached it before their cover file, as when a
    // download is added to it before its cover is written. Those read with
    // their cover are left alone, as are all the tracks of a library made
    // anew, with every album new to it
    WithoutCover,
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
    listed: Listed,
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
    // The folders are listed once, as albums can share one, and the folder
    // of an album has disc folders that show its cover files too
    let mut listings: HashMap<PathBuf, FolderCovers> = HashMap::new();
    let mut folder_files: HashMap<PathBuf, Vec<CoverFile>> = HashMap::new();
    let mut kept_covers = kept_covers(conn)?;
    for (id, album) in albums_of_tracks(conn, hidden)? {
        let (has_row, cover, kept_files) = match kept_covers.remove(&id) {
            Some(kept) => (true, kept.thumbnail, kept.cover_files),
            None => (false, None, None),
        };
        let first_track = path_from_bytes(album.first_track);
        // Thumbnails at another size than the grid shows them, or in the
        // format of older versions, are made again, or smaller
        let update = cover.as_deref().map_or(ThumbnailUpdate::Keep, |p| {
            sizes.update(thumbnail_size(Path::new(p), dir, &id))
        });
        // Albums without cover art have no thumbnail, so we only check that
        // the thumbnails we created are still there
        let removed = cover
            .as_deref()
            .is_some_and(|p| std::fs::metadata(p).is_err());
        // A cover file next to the tracks can change without them: it's
        // added, removed or replaced
        for folder in &album.folders {
            if !folder_files.contains_key(folder) {
                let files = cover_files_of(folder, &mut listings);
                folder_files.insert(folder.clone(), files);
            }
        }
        let first_folder = cue_sheet::real_path(&first_track).parent();
        let first_files = first_folder
            .and_then(|folder| folder_files.get(folder))
            .map_or(&[][..], Vec::as_slice);
        let other_files = album
            .folders
            .iter()
            .filter(|folder| Some(folder.as_path()) != first_folder)
            .filter_map(|folder| folder_files.get(folder))
            .flatten();
        let files = album_files_key(first_files, other_files);
        let cover_files = files.clone().unwrap_or_default();
        let listed = match (has_row, &kept_files) {
            (true, Some(kept)) if *kept != cover_files => Listed::All,
            // An older version kept no key, and went by the times of the
            // cover files: one newer than the thumbnail changed while it
            // wasn't running, which is noticed once, as the key is kept then
            (true, None)
                if cover.as_deref().is_some_and(|thumbnail| {
                    let files = album.folders.iter().filter_map(|f| folder_files.get(f));
                    cover_file_newer(files.flatten(), Path::new(thumbnail))
                }) =>
            {
                Listed::All
            }
            (false, _) if files.is_some() => Listed::WithoutCover,
            _ => Listed::No,
        };
        // An album without a thumbnail whose cover file couldn't be decoded
        // is tried again, as it was before keys were kept; the thumbnail is
        // made from the first track
        let retried = has_row && cover.is_none() && !first_files.is_empty();
        if !has_row
            || update == ThumbnailUpdate::Remake
            || removed
            || listed != Listed::No
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
                    listed,
                });
            }
        } else if let (ThumbnailUpdate::Shrink(size), Some(cover)) = (update, cover) {
            missing.push(CoverJob {
                id,
                first_track,
                larger: Some((PathBuf::from(cover), size)),
                cover_files,
                listed: Listed::No,
            });
        } else if has_row && kept_files.is_none() {
            unknown.push((id, cover_files));
        }
    }

    debug!("Library: generating {} album covers", missing.len());
    let mut made_covers = MadeCovers::default();
    // The albums whose tracks are listed, see `Listed`
    let mut listed_albums = Vec::new();

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
            if job.listed != Listed::No {
                listed_albums.push((job.id.as_str(), job.listed));
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
        for (id, listed) in listed_albums {
            let tracks = match listed {
                Listed::All => &mut made_covers.tracks_with_new_cover,
                Listed::WithoutCover => &mut made_covers.tracks_of_new_albums,
                Listed::No => continue,
            };
            let paths = album_tracks.query_map([id], |row| row.get::<_, Vec<u8>>(0))?;
            for path in paths {
                tracks.push(path_from_bytes(path?));
            }
        }
    }
    tx.commit()?;

    remove_unused_covers(conn, dir)?;
    Ok(made_covers)
}

// An album of the library, as `update_covers` looks at it
struct AlbumTracks {
    // The path of its first track, as SQLite orders paths, by their bytes
    first_track: Vec<u8>,
    // The folders of its tracks, as the tracks in another disc folder than
    // the first track's show the cover files in theirs
    folders: BTreeSet<PathBuf>,
}

// The albums of the tracks that aren't in the `hidden` folders, in the
// order of their ids. Every scan reads them all, even after one changed
// file, so they're read in one pass, and only what's new is copied
fn albums_of_tracks(
    conn: &Connection,
    hidden: &[PathBuf],
) -> rusqlite::Result<BTreeMap<String, AlbumTracks>> {
    let (hidden_sql, hidden_params) = hidden_condition("file", hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT album_id, path FROM tracks WHERE album_id IS NOT NULL{hidden_sql}"
    ))?;
    let mut rows = stmt.query(rusqlite::params_from_iter(hidden_params))?;
    let mut albums: BTreeMap<String, AlbumTracks> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id = row.get_ref(0)?.as_str()?;
        let path = row.get_ref(1)?.as_blob()?;
        let track = path_of_bytes(path);
        let folder = cue_sheet::real_path(&track).parent();
        if let Some(album) = albums.get_mut(id) {
            if path < album.first_track.as_slice() {
                album.first_track = path.to_vec();
            }
            if let Some(folder) = folder
                && !album.folders.contains(folder)
            {
                album.folders.insert(folder.to_path_buf());
            }
        } else {
            let album = AlbumTracks {
                first_track: path.to_vec(),
                folders: folder.map(Path::to_path_buf).into_iter().collect(),
            };
            albums.insert(id.to_string(), album);
        }
    }
    Ok(albums)
}

// An album's row of the covers table
struct KeptCover {
    // Its thumbnail, if one was made
    thumbnail: Option<String>,
    // The key of the cover files it was made with, if it's known
    cover_files: Option<String>,
}

fn kept_covers(conn: &Connection) -> rusqlite::Result<HashMap<String, KeptCover>> {
    conn.prepare("SELECT album_id, path, cover_files FROM covers")?
        .query_map([], |row| {
            let kept = KeptCover {
                thumbnail: row.get(1)?,
                cover_files: row.get(2)?,
            };
            Ok((row.get(0)?, kept))
        })?
        .collect()
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

// A cover file, with what tells that it changed
struct CoverFile {
    path: PathBuf,
    len: u64,
    // Since the Unix epoch, zero when it isn't known
    modified: Duration,
}

impl CoverFile {
    fn hash(&self, hasher: &mut Sha256, name: &[u8]) {
        hasher.update(name);
        hasher.update([0]);
        hasher.update(self.len.to_le_bytes());
        hasher.update(self.modified.as_nanos().to_le_bytes());
    }
}

// The cover files for a folder of tracks, in the order they're looked for,
// see `CoverCache::cover_files_for()`; the folders are listed once in
// `listings`
fn cover_files_of(dir: &Path, listings: &mut HashMap<PathBuf, FolderCovers>) -> Vec<CoverFile> {
    let list = |folder: &Path| {
        listings
            .entry(folder.to_path_buf())
            .or_insert_with(|| folder_covers(folder))
            .clone()
    };
    CoverCache::cover_files_listed(dir, list)
        .filter_map(|path| {
            let metadata = std::fs::metadata(&path).ok()?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .unwrap_or_default();
            Some(CoverFile {
                path,
                len: metadata.len(),
                modified,
            })
        })
        .collect()
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
fn cover_files_key(files: &[CoverFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    for file in files {
        let name = file.path.file_name().unwrap_or_default();
        file.hash(&mut hasher, name.as_encoded_bytes());
    }
    Some(utils::hex(&hasher.finalize()))
}

// The key of the cover files of an album's tracks: that of the files for
// the first track's folder, with the other files that the tracks in its
// other folders show, as those in another disc folder than the first
// track's show the cover files in theirs; `None` when there are none. A
// folder that shows no other files changes nothing, so that a folder
// without images of its own, or a disc folder that only shows those of
// the album's folder, as the first one does, can come or go without the
// cover files counting as changed. The other files are named with their
// folder, so that a cover moved from one disc folder to another changes
// the key
fn album_files_key<'a>(
    first: &[CoverFile],
    others: impl Iterator<Item = &'a CoverFile>,
) -> Option<String> {
    let mut others: Vec<&CoverFile> = others
        .filter(|file| !first.iter().any(|f| f.path == file.path))
        .collect();
    if others.is_empty() {
        return cover_files_key(first);
    }
    others.sort_by(|a, b| a.path.cmp(&b.path));
    others.dedup_by(|a, b| a.path == b.path);
    let mut hasher = Sha256::new();
    hasher.update(cover_files_key(first).as_deref().unwrap_or("-"));
    hasher.update([0]);
    for file in others {
        let folder = file.path.parent().and_then(Path::file_name);
        let name = file.path.file_name().unwrap_or_default();
        let mut named = folder.unwrap_or_default().as_encoded_bytes().to_vec();
        named.push(b'/');
        named.extend_from_slice(name.as_encoded_bytes());
        file.hash(&mut hasher, &named);
    }
    Some(utils::hex(&hasher.finalize()))
}

// Whether a cover file of an album is newer than its thumbnail, as older
// versions checked, before the key of the cover files was kept; a
// thumbnail that's gone is made again anyway
fn cover_file_newer<'a>(mut files: impl Iterator<Item = &'a CoverFile>, thumbnail: &Path) -> bool {
    let Ok(made) = std::fs::metadata(thumbnail).and_then(|m| m.modified()) else {
        return false;
    };
    let made = made.duration_since(UNIX_EPOCH).unwrap_or_default();
    files.any(|file| file.modified > made)
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
