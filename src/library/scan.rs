// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

pub(crate) enum ScanEvent {
    // The number of files read so far, out of the files that changed
    Progress(usize, usize),
    // The number of album covers made so far, out of those to make, after
    // the files are read
    Covers(usize, usize),
    Finished(ScanResult),
}

#[derive(Debug, Default)]
pub(crate) struct ScanResult {
    // The URIs of the tracks for the queue to read again: those that were
    // already known and changed, for their tags, and those of the albums
    // whose cover files changed without them, as when a cover is added
    // next to them, replaced or removed, for their cover. They're made in
    // the scan's thread, as there can be thousands of them
    pub(crate) reread: Vec<String>,
    // The URIs of the tracks of the albums new to the library with cover
    // files, for the queue to read again if it has them without a cover,
    // see `covers::Listed::WithoutCover`
    pub(crate) reread_without_cover: Vec<String>,
    // The number of tracks that are new, changed or gone
    pub(crate) n_changed: usize,
    // Whether covers of the albums were made again, which can keep their
    // names, as when a cover file next to the tracks changed; not the same
    // as the tracks of the albums with new cover files, as it's about the
    // grid's thumbnails
    pub(crate) covers_changed: bool,
    // What to watch for changes, see `folders_to_watch()`
    pub(crate) watch: WatchList,
}

// What to watch for changes in the music folders, as of a scan
#[derive(Debug, Default)]
pub(crate) struct WatchList {
    // The folders to watch for files that come, go or change
    pub(crate) folders: Vec<PathBuf>,
    // Whether a music folder that's there can't be watched, so that it's
    // read again when the albums are shown
    pub(crate) unwatched: bool,
    // The music folders that are there, and the folders that symbolic
    // links in them lead to, without symbolic links: a drive unmounted can
    // take one away, and a drive mounted in one adds to it. While one
    // isn't there, or a link leads nowhere, a drive mounted anywhere can
    // bring it back, as the folder a link leads to is only known once
    // it's there
    pub(crate) available: Vec<PathBuf>,
    pub(crate) unavailable: bool,
}

// The tags we keep for each track
#[derive(Default)]
pub(super) struct TrackTags {
    album_id: Option<String>,
    pub(super) album: Option<String>,
    album_artist: Option<String>,
    artist: Option<String>,
    artist_sort: Option<String>,
    pub(super) tag_dates: TagDates,
    disc: Option<u32>,
    track: Option<u32>,
    pub(super) title: Option<String>,
    // The text that searching looks into, as in the queue
    pub(super) search: Option<String>,
    // The composers, see `composer_entries()`
    composers: Option<String>,
    // The work the track performs, or a movement of, see `WorkEntry`
    pub(super) work: Option<WorkEntry>,
    // The conductors and performers, see `performer_entries()`
    pub(super) performers: Option<String>,
    // The first record label, and the first catalogue number
    pub(super) label: Option<String>,
    pub(super) catalog_number: Option<String>,
    // The values the fields of the search look into, as in the queue, see
    // `FieldParts`
    fields: Option<String>,
}

// The work of a track, as the library keeps it
#[derive(Debug, PartialEq)]
pub(super) struct WorkEntry {
    // What tells the work apart from others across albums: its MusicBrainz
    // id, or else its name and its composers, see `work_entry()`
    pub(super) key: String,
    // The name the queue shows it by
    pub(super) name: String,
    // The full title, when it's not the name
    title: Option<String>,
    // The work it's part of, or the collection it's in
    collection: Option<String>,
    // Its composers, as in `composer_entries()`
    pub(super) composers: Option<String>,
    // Where the movement is in the work, e.g. "1.3.3", or "" if that isn't
    // known; `None` if the track performs the whole work
    pub(super) order: Option<String>,
    // For an arrangement, the title of the work it arranges, and what it
    // is, or "" if MusicBrainz doesn't say, see `Arrangement`
    pub(super) arrangement_of: Option<String>,
    pub(super) arrangement: Option<String>,
    // The MusicBrainz id of the work an arrangement arranges, if the plugin
    // gave it
    pub(super) arrangement_of_id: Option<String>,
    // The MusicBrainz ids of the parts of the work it performs, at every
    // level below the work, on a line each, see `WorkLabels::part_ids`
    pub(super) part_ids: Option<String>,
}

// Updates the database with the tracks in the music folders, and makes the
// covers of the albums for the grid at the sizes `thumbnail_sizes` gives;
// this blocks, so it should be called from a separate thread
pub(crate) fn scan(
    folders: &[MusicFolder],
    thumbnail_sizes: ThumbnailSizes,
    sender: &Sender<ScanEvent>,
) {
    let now = Instant::now();
    let roots = roots(folders);
    debug!("Library: reading {roots:?}");
    let mut trees = read_roots(&roots);
    let paths = trees
        .iter_mut()
        .flat_map(|tree| std::mem::take(&mut tree.files))
        .filter_map(|file| file.path())
        .collect();
    let unread: Vec<PathBuf> = trees
        .iter_mut()
        .flat_map(|tree| std::mem::take(&mut tree.unread))
        .collect();
    let mut result = with_connection(|conn| {
        let (mut result, stale_albums) = scan_with(conn, &roots, paths, &unread, sender)?;
        let made = update_covers(
            conn,
            &covers_dir(),
            &stale_albums,
            &roots.unavailable,
            thumbnail_sizes,
            sender,
        )?;
        result.covers_changed = made.count > 0;
        result.reread.extend(uris(&made.tracks_with_new_cover));
        result.reread_without_cover = uris(&made.tracks_of_new_albums);
        Ok(result)
    })
    .unwrap_or_else(|e| {
        warn!("Unable to scan the library: {e}");
        ScanResult::default()
    });
    debug!("Library scan: {} ms", now.elapsed().as_millis());
    // Even when the database couldn't be read, the folders are watched
    result.watch = folders_to_watch(folders, &roots, &trees);

    let _ = sender.try_send(ScanEvent::Finished(result));
}

// The URIs of the files, by which the queue knows its tracks
fn uris(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| gio::File::for_path(path).uri().to_string())
        .collect()
}

// What the scan reads in each music folder that's there, in their order,
// see `scan_reads()`; this blocks
pub(super) fn read_roots(roots: &Roots) -> Vec<utils::FolderTree> {
    roots
        .available
        .iter()
        .map(|root| {
            utils::read_folder_tree(&gio::File::for_path(root), true, &|path| {
                scan_reads(path, roots)
            })
        })
        .collect()
}

// The folders of the library to watch for files that come, go or change:
// those in the music folders on this computer's disks, with the folders in
// them and those that symbolic links in them lead to, as the scan reads
// them, but not those that are hidden or left out, which it doesn't.
// Changes in a folder on a network share can be made from other
// computers, which aren't told about, and the folders that came through
// the document portal aren't watched either. Also the music folders and
// the folders that links lead to, for the drives mounted and unmounted,
// see `WatchList`. The `trees` are those of `read_roots()`. This blocks
pub(super) fn folders_to_watch(
    folders: &[MusicFolder],
    roots: &Roots,
    trees: &[utils::FolderTree],
) -> WatchList {
    let mut list = WatchList {
        unavailable: !roots.unavailable.is_empty(),
        ..WatchList::default()
    };
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for (root, tree) in roots.available.iter().zip(trees) {
        list.available.push(canonical(root));
        let portal = folders
            .iter()
            .any(|folder| folder.path == *root && folder.host.is_some());
        if portal || is_remote(root) {
            debug!("Library: not watching {root:?}");
            list.unwatched = true;
            continue;
        }
        // A link to a drive that isn't mounted leads nowhere until it is
        list.unavailable |= tree.broken_links;
        let mut remote = Vec::new();
        for link in tree.links.iter().filter_map(gio::File::path) {
            list.available.push(canonical(&link));
            if is_remote(&link) {
                debug!("Library: not watching {link:?}");
                list.unwatched = true;
                remote.push(link);
            }
        }
        list.folders.extend(
            tree.folders
                .iter()
                .filter_map(gio::File::path)
                .filter(|folder| !remote.iter().any(|link| folder.starts_with(link))),
        );
    }
    list
}

fn is_remote(path: &Path) -> bool {
    gio::File::for_path(path)
        .query_filesystem_info(
            gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
            None::<&gio::Cancellable>,
        )
        .is_ok_and(|info| info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE))
}

// Whether the scan reads a file or a folder it comes to in the music
// folders: not a hidden one, nor a folder left out
fn scan_reads(path: &Path, roots: &Roots) -> bool {
    is_library_entry(path) && !roots.excluded.iter().any(|excluded| excluded == path)
}

// When the file of tracks last changed, and its size: with the audio
// files that a CUE sheet splits, the last change of any, and their sizes
// together
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stamp {
    mtime: i64,
    ctime: i64,
    size: i64,
}

// A file that tracks come from: an audio file, or a CUE sheet with the
// audio files it splits
struct Source {
    path: PathBuf,
    stamp: Stamp,
    // When it was created; a new track counts as added then, so that the
    // tracks that were in the music folders before the library existed, or
    // before their folder was added, are sorted by when they were added too
    created: i64,
    // The audio files a sheet splits
    audio: Vec<PathBuf>,
}

impl Source {
    fn new(path: PathBuf, audio: Vec<PathBuf>) -> std::io::Result<Self> {
        let seconds = |t: std::io::Result<std::time::SystemTime>| {
            t.ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        };
        let mut stamp = Stamp {
            mtime: 0,
            ctime: 0,
            size: 0,
        };
        let mut created = i64::MAX;
        for file in std::iter::once(&path).chain(&audio) {
            let metadata = std::fs::metadata(file)?;
            let mtime = seconds(metadata.modified()).unwrap_or_default();
            stamp.mtime = stamp.mtime.max(mtime);
            stamp.ctime = stamp.ctime.max(change_time(&metadata));
            stamp.size += metadata.len() as i64;
            created = created.min(seconds(metadata.created()).unwrap_or(mtime));
        }
        Ok(Self {
            path,
            stamp,
            created,
            audio,
        })
    }
}

// Whether a file couldn't be read because it isn't there anymore, or a
// folder on its path isn't, rather than because it can't be read now
fn is_not_there(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

// Updates the tracks from the files of the music folders, the `paths` of
// `read_roots()`, and returns the albums whose cover should be looked for
// again. The files in the `unread` folders, which couldn't be read whole,
// keep their tracks
pub(super) fn scan_with(
    conn: &mut Connection,
    roots: &Roots,
    paths: Vec<PathBuf>,
    unread: &[PathBuf],
    sender: &Sender<ScanEvent>,
) -> rusqlite::Result<(ScanResult, HashSet<String>)> {
    // The CUE sheets that split audio files have their tracks, in place
    // of the files
    let claims = Claims::new(paths.iter().map(PathBuf::as_path));

    // When each file changed at the last scan, and when its tracks were
    // added
    let mut known = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT file, mtime, ctime, size, MIN(added) FROM tracks GROUP BY file")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                Stamp {
                    mtime: row.get(1)?,
                    ctime: row.get(2)?,
                    size: row.get(3)?,
                },
                row.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (file, stamp, added) = row?;
            known.insert(file, (stamp, added));
        }
    }
    // When the files were added whose tracks an older version of the
    // schema dropped, see `keep_added_times()`
    let kept: HashMap<Vec<u8>, i64> = conn
        .prepare("SELECT file, added FROM added_times")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    // Find the files that are new or changed since the last scan
    let mut present = HashSet::new();
    let mut changed = Vec::new();
    for path in paths {
        let audio: Vec<PathBuf> = if cue_sheet::is_cue_path(&path) {
            let audio: Vec<PathBuf> = claims
                .tracks_of(&path)
                .into_iter()
                .map(|(_, audio)| audio)
                .unique()
                .map(Path::to_path_buf)
                .collect();
            if audio.is_empty() {
                continue;
            }
            audio
        } else if utils::is_audio_path(&path) && claims.sheet_of(&path).is_none() {
            Vec::new()
        } else {
            continue;
        };
        let key = path_bytes(&path).to_vec();
        let source = match Source::new(path, audio) {
            Ok(source) => source,
            // A file that's there, but can't be read now, e.g. on a network
            // share that stopped answering, keeps its tracks as they are
            Err(e) => {
                if !is_not_there(&e) {
                    warn!("Unable to read {}: {e}", path_of_bytes(&key).display());
                    present.insert(key);
                }
                continue;
            }
        };
        if known.get(&key).map(|(stamp, _)| stamp) != Some(&source.stamp) {
            changed.push(source);
        }
        present.insert(key);
    }

    debug!(
        "Library: {} files of tracks, {} new or changed",
        present.len(),
        changed.len()
    );

    // The albums whose cover should be looked for again
    let mut stale_albums = HashSet::new();
    let mut updated = Vec::new();
    let mut n_changed = 0;

    let total = changed.len();
    for (i, chunk) in changed.chunks(200).enumerate() {
        let tx = conn.transaction()?;
        {
            let mut old_albums = tx.prepare(
                "SELECT DISTINCT album_id FROM tracks WHERE file = ?1 AND album_id IS NOT NULL",
            )?;
            let mut delete = tx.prepare("DELETE FROM tracks WHERE file = ?1")?;
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO tracks
                 (path, file, mtime, ctime, size, album_id, album, album_artist, artist,
                  artist_sort, disc, track, title, search, added, composers, work_key, work,
                  work_title, work_collection, work_composers, movement_order, performers,
                  label, catalog_number, work_arrangement_of, work_arrangement,
                  work_arrangement_of_id, work_part_ids, fields, release_sort_date,
                  date_tag_sort_date, original_sort_date, recorded_sort_date)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                         ?31, ?32, ?33, ?34)",
            )?;
            // The files are read a few at a time, and written here, in the
            // transaction, as they come
            let read = |source: &Source| {
                // A file that makes the tag reader panic is kept as a track
                // without tags, which is in no album, so that the scan goes
                // on, and the file isn't read again until it changes
                let read_file = || {
                    if source.audio.is_empty() {
                        read_tags(&source.path)
                    } else {
                        claims
                            .sheets
                            .get(&source.path)
                            .map_or_else(Vec::new, |sheet| {
                                read_sheet_tags(sheet, claims.tracks_of(&source.path))
                            })
                    }
                };
                utils::guarded(source.path.display(), read_file, || {
                    vec![(source.path.clone(), TrackTags::default())]
                })
            };
            utils::in_threads(chunk, utils::READING_THREADS, read, |i, tracks| {
                let source = &chunk[i];
                let key = path_bytes(&source.path);
                for id in old_albums.query_map([key], |row| row.get::<_, String>(0))? {
                    stale_albums.insert(id?);
                }
                delete.execute([key])?;

                // A changed track keeps the time it was added, as do the
                // tracks of an audio file that a CUE sheet splits now, and
                // those of a file read again for a new version of the schema
                let was_known = known.contains_key(key);
                let added = std::iter::once(&source.path)
                    .chain(&source.audio)
                    .flat_map(|path| {
                        let file = path_bytes(path);
                        let known = known.get(file).map(|(_, added)| *added);
                        known.into_iter().chain(kept.get(file).copied())
                    })
                    .min()
                    .unwrap_or(source.created);

                let text = |date: Option<TagDate>| date.map(TagDate::sort_text);
                for (path, tags) in tracks {
                    let work = tags.work.as_ref();
                    insert.execute(params![
                        path_bytes(&path),
                        key,
                        source.stamp.mtime,
                        source.stamp.ctime,
                        source.stamp.size,
                        tags.album_id,
                        tags.album,
                        tags.album_artist,
                        tags.artist,
                        tags.artist_sort,
                        tags.disc,
                        tags.track,
                        tags.title,
                        tags.search,
                        added,
                        tags.composers,
                        work.map(|w| &w.key),
                        work.map(|w| &w.name),
                        work.and_then(|w| w.title.as_ref()),
                        work.and_then(|w| w.collection.as_ref()),
                        work.and_then(|w| w.composers.as_ref()),
                        work.and_then(|w| w.order.as_ref()),
                        tags.performers,
                        tags.label,
                        tags.catalog_number,
                        work.and_then(|w| w.arrangement_of.as_ref()),
                        work.and_then(|w| w.arrangement.as_ref()),
                        work.and_then(|w| w.arrangement_of_id.as_ref()),
                        work.and_then(|w| w.part_ids.as_ref()),
                        tags.fields,
                        text(tags.tag_dates.release),
                        text(tags.tag_dates.date),
                        text(tags.tag_dates.original),
                        text(tags.tag_dates.recorded),
                    ])?;
                    n_changed += 1;
                    // A changed track might have new cover art; new tracks
                    // don't make the cover of their album stale, so that the
                    // covers are kept when the whole library is read again
                    if was_known {
                        stale_albums.extend(tags.album_id);
                        updated.push(path);
                    }
                }
                Ok::<_, rusqlite::Error>(())
            })?;
        }
        tx.commit()?;

        let done = (i * 200 + chunk.len()).min(total);
        let _ = sender.try_send(ScanEvent::Progress(done, total));
    }

    // Forget the files that are gone, or left out, but not those of the
    // folders that aren't there, e.g. on a drive that isn't plugged in, which
    // aren't read again once they're back: the music folders that weren't
    // there when the scan started, those that went while it read them, as
    // when a drive is unplugged, and the folders that couldn't be read whole
    let kept_folders: Vec<&Path> = roots
        .unavailable
        .iter()
        .chain(roots.available.iter().filter(|root| !is_available(root)))
        .chain(unread)
        .map(PathBuf::as_path)
        .collect();
    let is_gone = |file: &&[u8]| {
        !present.contains(*file)
            && (kept_folders.is_empty() || {
                let path = path_of_bytes(file);
                !kept_folders.iter().any(|folder| path.starts_with(folder))
            })
    };
    let gone: Vec<&[u8]> = known.keys().map(Vec::as_slice).filter(is_gone).collect();
    let kept_gone: Vec<&[u8]> = kept.keys().map(Vec::as_slice).filter(is_gone).collect();
    if !gone.is_empty() || !kept.is_empty() {
        let tx = conn.transaction()?;
        if !gone.is_empty() {
            debug!("Library: {} files of tracks removed", gone.len());
            let mut old_albums = tx.prepare(
                "SELECT DISTINCT album_id FROM tracks WHERE file = ?1 AND album_id IS NOT NULL",
            )?;
            let mut delete = tx.prepare("DELETE FROM tracks WHERE file = ?1")?;
            for file in gone {
                for id in old_albums.query_map([file], |row| row.get::<_, String>(0))? {
                    stale_albums.insert(id?);
                }
                n_changed += delete.execute([file])?;
            }
        }
        if !kept.is_empty() {
            let mut forget = tx.prepare("DELETE FROM added_times WHERE file = ?1")?;
            for file in kept_gone {
                forget.execute([file])?;
            }
            // The tracks of the files read again have their times now
            tx.execute(
                "DELETE FROM added_times WHERE file IN (SELECT file FROM tracks)",
                [],
            )?;
        }
        tx.commit()?;
    }

    Ok((
        ScanResult {
            reread: uris(&updated),
            n_changed,
            ..ScanResult::default()
        },
        stale_albums,
    ))
}

// When the file last changed, in nanoseconds: unlike the modification
// time, taggers can't keep it from changing when they write the tags, as
// some do to keep the files in the order they were added
fn change_time(metadata: &std::fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata
            .ctime()
            .saturating_mul(1_000_000_000)
            .saturating_add(metadata.ctime_nsec())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

// An audio file's tags, and the items lofty leaves out of them, which have
// the CUE sheet in the tags, with the properties and without the covers
fn read_tags_only(path: &Path) -> Option<FileTags> {
    read_file_tags(path, ReadOptions::SCAN)
}

// The tracks of an audio file: the file, or the tracks that the CUE sheet
// in its tags splits it into
fn read_tags(path: &Path) -> Vec<(PathBuf, TrackTags)> {
    let Some(tags) = read_tags_only(path) else {
        return vec![(path.to_path_buf(), TrackTags::default())];
    };
    let file = Audio::of(&tags);
    let FileTags { tag, extra, .. } = tags;

    let sheet = extra
        .cue_sheet
        .as_deref()
        .and_then(|text| SplitSheet::embedded(path, text));
    if let Some(sheet) = sheet {
        let album_tag = cue_sheet::album_tag(Some(&tag), &sheet.sheet);
        return sheet
            .tracks()
            .map(|(track, _)| cue_track_tags(&sheet, &album_tag, track, &file))
            .collect();
    }

    // A file without tags is in the album of its folder too
    let tags = TrackTags::from_tag(&tag, path, Some(path), &extra, file.whole());
    vec![(path.to_path_buf(), tags)]
}

// What the fields take from an audio file
#[derive(Default)]
struct Audio {
    format: Option<Cow<'static, str>>,
    properties: AudioProperties,
    duration: Duration,
}

impl Audio {
    fn of(tags: &FileTags) -> Self {
        Self {
            format: tags.format.clone(),
            properties: AudioProperties::of(&tags.properties),
            duration: tags.properties.duration(),
        }
    }

    // The format and the properties of the whole file
    fn whole(&self) -> (Option<&str>, Option<AudioProperties>) {
        (self.format.as_deref(), Some(self.properties))
    }

    // Those of a track of a CUE sheet, as long as its part of the file
    fn part(&self, track: &CueTrack) -> (Option<&str>, Option<AudioProperties>) {
        let length = track.length(self.duration);
        (
            self.format.as_deref(),
            Some(self.properties.with_length(length)),
        )
    }
}

// The tracks that a ".cue" file splits audio files into
fn read_sheet_tags(
    sheet: &SplitSheet,
    tracks: Vec<(&CueTrack, &Path)>,
) -> Vec<(PathBuf, TrackTags)> {
    let mut files = HashMap::new();
    tracks
        .into_iter()
        .map(|(track, audio)| {
            let (album_tag, file) = files.entry(audio).or_insert_with(|| {
                let file = read_tags_only(audio);
                let album_tag =
                    cue_sheet::album_tag(file.as_ref().map(|file| &file.tag), &sheet.sheet);
                (album_tag, file.as_ref().map(Audio::of).unwrap_or_default())
            });
            cue_track_tags(sheet, album_tag, track, file)
        })
        .collect()
}

fn cue_track_tags(
    sheet: &SplitSheet,
    album_tag: &Tag,
    track: &CueTrack,
    file: &Audio,
) -> (PathBuf, TrackTags) {
    let tag = cue_sheet::track_tag(album_tag, &sheet.sheet, track);
    // The album is in the folder of the sheet, and the items that lofty
    // doesn't read are about the whole audio file
    let extra = ExtraComments::default();
    let tags = TrackTags::from_tag(&tag, &sheet.path, None, &extra, file.part(track));
    (sheet.location(track), tags)
}

impl TrackTags {
    // The title of a track without one is the name of its `file`, which
    // is searched like the queue searches it; the tracks of CUE sheets
    // have none. The items that lofty doesn't read, like the sort names of
    // the composers and the "Work Hierarchy" tag, are in `extra`
    pub(super) fn from_tag(
        tag: &Tag,
        path: &Path,
        file: Option<&Path>,
        extra: &ExtraComments,
        (format, properties): (Option<&str>, Option<AudioProperties>),
    ) -> Self {
        let get = |key| tag.get_string(key).map(|s| s.trim().to_string());

        let album = get(ItemKey::AlbumTitle).filter(|s| !s.is_empty());
        let album_artist = tag
            .get_string(ItemKey::AlbumArtist)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let artist = tag.artist();
        let artist = artist.as_deref().map(str::trim).filter(|s| !s.is_empty());
        // The album goes by the first artist, but the performers, the search
        // and the work of the track take all of them, as the queue does, so
        // that both find the same tracks: the tags of a duet can name each
        // singer as an artist of their own
        let artists = track_artist(tag);
        let artists = artists.as_deref().map(str::trim).filter(|s| !s.is_empty());

        // The release id from MusicBrainz tells apart albums with the same
        // name; without it, we use the album artist, so the discs of an
        // album in separate folders stay together. Compilations often have
        // no album artist, and a different artist for each track, so we fall
        // back to the folder the album is in. Tracks without an album are
        // grouped by that folder, as an album without a name, which the
        // tags can't have
        let mbid = get(ItemKey::MusicBrainzReleaseId).filter(|s| album.is_some() && is_mbid(s));
        let album_id = mbid.unwrap_or_else(|| {
            let mut hasher = Sha256::new();
            hasher.update(album.as_deref().unwrap_or_default());
            hasher.update([0]);
            match (album.as_ref().and(album_artist), album_folder(path)) {
                (Some(artist), _) => hasher.update(artist),
                (None, Some(folder)) => {
                    hasher.update(folder.as_os_str().as_encoded_bytes());
                }
                (None, None) => (),
            }
            utils::hex(&hasher.finalize())
        });
        // The album of a track without one is shown as its folder's name,
        // in the grid as in the queue
        let folder_album = album.is_none().then(|| folder_album_title(path)).flatten();

        let credits = TrackCredits::from_tag(tag, extra);

        // As `Track::with_search_key()` makes it, so that searching the albums
        // matches the same tracks as searching the queue
        let title = track_title(tag);
        let searched_title = title
            .as_deref()
            .map(Cow::Borrowed)
            .or_else(|| file.map(|file| title_from_file_name(file).into()));
        let search = search_key(
            artists,
            searched_title.as_deref(),
            &track_search_text(tag, &credits),
            folder_album.as_deref(),
        );
        let performers = credits.listed_with_artists(artists);
        let fields = FieldParts {
            tag,
            credits: &credits,
            performers: &performers,
            title: searched_title.as_deref(),
            album: album.as_deref().or(folder_album.as_deref()),
            format,
            properties,
        }
        .encode();
        let performers = performer_entries(&performers);
        TrackTags {
            album_id: Some(album_id),
            album: album.or(folder_album),
            // After the id, which must not change
            album_artist: album_artist.map(plain_commas),
            artist: artist.map(plain_commas),
            artist_sort: credits.album_artist_sort.clone(),
            tag_dates: credits.tag_dates(),
            disc: tag.disk(),
            track: tag.track(),
            title,
            search: Some(search),
            composers: composer_entries(&credits.composers, &credits.composer_sorts),
            work: work_entry(&credits, artists),
            performers,
            label: credits.labels.into_iter().next(),
            catalog_number: credits.catalog_numbers.into_iter().next(),
            fields: Some(fields),
        }
    }
}

// The work a track performs, or a movement of. The work is told apart by
// its MusicBrainz id: the one the "Work Hierarchy" plugin picks, or else,
// for a track of a whole work, the work it's linked to, which for a
// movement would be the movement's. An arrangement of a single movement,
// which the tags make a movement of the work, is told apart by the
// arrangement it's linked to, as its name would make it one with the
// recordings of the work without MusicBrainz ids. Without one, works with
// the same name and composers are the same, as the queue's headers take
// them to be
fn work_entry(credits: &TrackCredits, artist: Option<&str>) -> Option<WorkEntry> {
    let name = credits.work.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    let is_movement = credits.is_movement();

    let composers = credited_work_composers(credits, artist);
    let key = credits
        .work_id
        .clone()
        .or_else(|| {
            credits
                .musicbrainz
                .work
                .as_ref()
                .filter(|_| !is_movement || credits.work_arrangement.is_some())
                .cloned()
        })
        .unwrap_or_else(|| named_work_key(name, &composers));
    let work_names: Vec<String> = composers.iter().map(|c| c.name.clone()).collect();
    let work_sorts: Vec<String> = composers.into_iter().map(|c| c.sort).collect();
    let order = credits
        .movement_order
        .as_ref()
        .filter(|_| is_movement)
        .map(|order| order.iter().join("."));
    Some(WorkEntry {
        key,
        name: name.to_string(),
        title: credits.work_title.clone(),
        collection: credits.work_collection.clone(),
        composers: composer_entries(&work_names, &work_sorts),
        order,
        arrangement_of: credits
            .work_arrangement
            .as_ref()
            .map(|arrangement| arrangement.of.clone()),
        arrangement: credits
            .work_arrangement
            .as_ref()
            .map(|arrangement| arrangement.label.clone().unwrap_or_default()),
        arrangement_of_id: credits
            .work_arrangement
            .as_ref()
            .and_then(|arrangement| arrangement.of_id.clone()),
        part_ids: (!credits.work_part_ids.is_empty()).then(|| credits.work_part_ids.join("\n")),
    })
}
