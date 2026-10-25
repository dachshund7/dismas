// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading the tracks of files, in a few threads

use super::*;

// The longest a track can be. A file can report a far longer one in its
// header, e.g. an Ogg file cut off in the middle of a page, and the clock
// times of GStreamer and MPRIS can't hold the longest; such a length counts
// as unknown. Audiobooks in a single file run to some tens of hours
const MAX_DURATION: Duration = Duration::from_secs(1000 * 60 * 60);

// The length of a track in seconds, 0 if it's unknown
fn sane_duration(duration: Duration) -> u64 {
    if duration > MAX_DURATION {
        warn!("Ignoring a length of {} s", duration.as_secs());
        0
    } else {
        duration.as_secs()
    }
}

// The id of a track, which names the cache of its waveform
fn track_uuid(display_name: &str, number: Option<u32>, tags: [&Option<String>; 3]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(display_name);
    // The tracks of a CUE sheet share their file, and can share a title
    if let Some(number) = number {
        hasher.update(format!("/track{number:02}"));
    }
    for tag in tags.into_iter().flatten() {
        hasher.update(tag);
    }
    utils::hex(&hasher.finalize())
}

// An audio file, with its tags and properties
struct FileRead {
    path: PathBuf,
    tagged_file: TaggedFile,
    extra: ExtraComments,
    display_name: Option<String>,
    size: Option<u64>,
    // The path outside the sandbox, if the file came through the document
    // portal
    host_path: Option<PathBuf>,
}

impl FileRead {
    fn read(path: &Path) -> Option<Self> {
        let file_probe = match Probe::open(path) {
            Ok(p) => p,
            Err(e) => {
                warn!("Unable to open file {path:?}: {e}");
                return None;
            }
        };

        let (tagged_file, extra) = match file_probe.guess_file_type() {
            Ok(t) => match read_tagged_file(t, ParseOptions::new()) {
                Ok(f) => f,
                Err(e) => {
                    warn!("Unable to read parsed file {path:?}: {e}");
                    return None;
                }
            },
            Err(e) => {
                warn!("Unable to parse file type {path:?}: {e}");
                return None;
            }
        };

        let info = gio::File::for_path(path)
            .query_info(
                &format!("standard::display-name,standard::size,{ATTRIBUTE_HOST_PATH}"),
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .ok();

        Some(Self {
            path: path.to_path_buf(),
            tagged_file,
            extra,
            display_name: info.as_ref().map(|info| info.display_name().to_string()),
            size: info
                .as_ref()
                .and_then(|info| u64::try_from(info.size()).ok()),
            host_path: info
                .as_ref()
                .and_then(|info| info.attribute_as_string(ATTRIBUTE_HOST_PATH))
                .map(|p| PathBuf::from(p.as_str())),
        })
    }

    fn details(&self, replay_gain: ReplayGain) -> FileDetails {
        let properties = self.tagged_file.properties();
        FileDetails {
            format: format_name(self.tagged_file.file_type()),
            path: self.host_path.clone().or_else(|| Some(self.path.clone())),
            size: self.size,
            bitrate: properties.audio_bitrate().filter(|b| *b > 0),
            sample_rate: properties.sample_rate().filter(|r| *r > 0),
            bit_depth: properties.bit_depth().filter(|d| *d > 0),
            channels: properties.channels().filter(|c| *c > 0),
            replay_gain,
        }
    }
}

// An audio file that a CUE sheet splits, with what its tracks take from it
struct CueFile {
    path: PathBuf,
    // The tags of the album, without the pictures, which the cover is
    // read from
    tag: Tag,
    cover: AlbumCover,
    duration: Duration,
    display_name: Option<String>,
    details: FileDetails,
}

impl CueFile {
    fn new(sheet: &cue_sheet::CueSheet, read: &FileRead) -> Self {
        let file_tag = read
            .tagged_file
            .primary_tag()
            .or_else(|| read.tagged_file.first_tag());
        let mut tag = cue_sheet::album_tag(file_tag, sheet);
        // All the tracks have the same album, so they share the cover
        let cover = CoverCache::cover_art(&read.path, &tag);
        while !tag.pictures().is_empty() {
            tag.remove_picture(0);
        }
        Self {
            path: read.path.clone(),
            tag,
            cover,
            duration: read.tagged_file.properties().duration(),
            display_name: read.display_name.clone(),
            details: read.details(ReplayGain::default()),
        }
    }
}

// Values computed once, by the first thread that needs each
struct Memo<K, V>(Mutex<HashMap<K, Arc<OnceLock<V>>>>);

impl<K, V> Default for Memo<K, V> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<K: Eq + Hash, V: Clone> Memo<K, V> {
    fn get(&self, key: K, init: impl FnOnce() -> V) -> V {
        let slot = self.0.lock().unwrap().entry(key).or_default().clone();
        slot.get_or_init(init).clone()
    }
}

// What the threads reading a list of files share: which audio files the
// CUE sheets among the files split, and the sheets and the audio files
// that several tracks come from, which are read once
#[derive(Default)]
pub(super) struct FileCache {
    claims: Claims,
    sheets: Memo<PathBuf, Option<Arc<SplitSheet>>>,
    // By sheet and audio file
    cue_files: Memo<(PathBuf, PathBuf), Option<Arc<CueFile>>>,
    // The ".cue" files in a folder that split audio files
    folders: Memo<PathBuf, Arc<Vec<Arc<SplitSheet>>>>,
}

impl FileCache {
    // For a list of files, given which CUE sheets among them split which
    // audio files
    pub(super) fn new(claims: Claims) -> Self {
        Self {
            claims,
            ..Default::default()
        }
    }

    // A ".cue" file, or the sheet in the tags of an audio file
    fn sheet(&self, path: &Path) -> Option<Arc<SplitSheet>> {
        if let Some(sheet) = self.claims.sheets.get(path) {
            return Some(sheet.clone());
        }
        self.sheets.get(path.to_path_buf(), || {
            if cue_sheet::is_cue_path(path) {
                return SplitSheet::read(path).map(Arc::new);
            }
            let read = FileRead::read(path)?;
            let sheet = Arc::new(SplitSheet::embedded(
                path,
                read.extra.cue_sheet.as_deref()?,
            )?);
            // Its tracks take the rest from the file that was just read
            let key = (path.to_path_buf(), path.to_path_buf());
            self.cue_files
                .get(key, || Some(Arc::new(CueFile::new(&sheet.sheet, &read))));
            Some(sheet)
        })
    }

    fn cue_file(&self, sheet: &SplitSheet, audio: &Path) -> Option<Arc<CueFile>> {
        let key = (sheet.path.clone(), audio.to_path_buf());
        self.cue_files.get(key, || {
            let read = FileRead::read(audio)?;
            Some(Arc::new(CueFile::new(&sheet.sheet, &read)))
        })
    }

    // The first ".cue" file next to an audio file that splits it
    fn folder_sheet(&self, audio: &Path) -> Option<Arc<SplitSheet>> {
        let dir = audio.parent()?;
        self.folders
            .get(dir.to_path_buf(), || {
                Arc::new(cue_sheet::folder_sheets(dir))
            })
            .iter()
            .find(|sheet| sheet.splits(audio))
            .cloned()
    }
}

// Decoding a cover mostly waits for its sandboxed loader, so a few at a
// time go faster, even on fewer cores
const MAX_LOADING_THREADS: usize = 4;

// Reading the tags and decoding the covers takes a while, so the tracks
// are read in a few threads. They are sent in the order of the files, all
// the tracks of a file at once, which can be none; the threads stop once
// the receiver is dropped. With `placeholders`, for the tracks of a
// playlist, a file that isn't there is a track that `TrackData::missing()`
// makes, so that saving the playlist doesn't lose it
pub fn load_tracks(
    files: Vec<gio::File>,
    placeholders: bool,
) -> async_channel::Receiver<Vec<TrackData>> {
    let (sender, receiver) = async_channel::unbounded();

    let uris: Arc<Vec<String>> = Arc::new(files.iter().map(|f| f.uri().to_string()).collect());
    let paths: Vec<PathBuf> = files.iter().filter_map(FileExt::path).collect();
    std::thread::spawn(move || {
        // Which sheets split the audio files, before any is read
        let cache = Arc::new(FileCache::new(Claims::new(
            paths.iter().map(PathBuf::as_path),
        )));

        let next = Arc::new(AtomicUsize::new(0));
        let (read_sender, read_receiver) = std::sync::mpsc::channel();
        let n_threads = MAX_LOADING_THREADS.min(uris.len()).max(1);
        for _ in 0..n_threads {
            let uris = Arc::clone(&uris);
            let next = Arc::clone(&next);
            let cache = Arc::clone(&cache);
            let read_sender = read_sender.clone();
            std::thread::spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(uri) = uris.get(index) else {
                        break;
                    };
                    // A file whose tags make the reader panic is skipped,
                    // rather than leaving the tracks after it unread
                    let mut data = utils::guarded(uri, || TrackData::read(uri, &cache), Vec::new);
                    if data.is_empty() && placeholders {
                        let file = gio::File::for_uri(uri);
                        if file
                            .path()
                            .is_some_and(|path| !cue_sheet::location_exists(&path))
                        {
                            data.push(TrackData::missing(file));
                        }
                    }
                    if read_sender.send((index, data)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(read_sender);

        // The tracks read out of order wait for the ones before them
        let mut waiting = BTreeMap::new();
        let mut next_index = 0;
        for (index, data) in read_receiver {
            waiting.insert(index, data);
            while let Some(data) = waiting.remove(&next_index) {
                if sender.try_send(data).is_err() {
                    return;
                }
                next_index += 1;
            }
        }
    });

    receiver
}

impl TrackData {
    // Reads the tracks of a file: an audio file, which a CUE sheet next to
    // it or in its tags can split into tracks, the tracks of a sheet, or a
    // track of a sheet. This blocks, so it must not be called on the main
    // thread
    pub(super) fn read(uri: &str, files: &FileCache) -> Vec<Self> {
        let now = Instant::now();

        let file = gio::File::for_uri(uri);
        // Files without a local path, like the ones in the trash or on the
        // web, can't be read by lofty
        let Some(path) = file.path() else {
            warn!("Unable to find a local path for {uri}");
            return Vec::new();
        };

        let tracks = if let Some((sheet, number)) = cue_sheet::parse_location(&path) {
            let Some(sheet) = files.sheet(sheet) else {
                warn!("Unable to read the CUE sheet of {uri}");
                return Vec::new();
            };
            let Some(track) = sheet.track(number) else {
                warn!("Unable to find track {number} in {:?}", sheet.path);
                return Vec::new();
            };
            Self::read_cue_tracks(&sheet, vec![track], files)
        } else if cue_sheet::is_cue_path(&path) {
            // The files that a sheet before it in the list splits have that
            // sheet's tracks
            match files.claims.sheets.get(&path) {
                Some(sheet) => Self::read_cue_tracks(sheet, files.claims.tracks_of(&path), files),
                None => Vec::new(),
            }
        } else {
            Self::read_audio_file(file, &path, files)
        };

        debug!(
            "Tracks of {uri}: {}, loading time: {} ms",
            tracks.len(),
            now.elapsed().as_millis()
        );
        tracks
    }

    fn read_audio_file(file: gio::File, path: &Path, files: &FileCache) -> Vec<Self> {
        let (mime_type, _) = gio::content_type_guess(Some(path), None);
        if !mime_type.starts_with("audio") || mime_type.ends_with("x-mpegurl") {
            return Vec::new();
        }

        // A sheet among the files has its tracks
        if files.claims.sheet_of(path).is_some() {
            return Vec::new();
        }
        if let Some(sheet) = files.folder_sheet(path) {
            let tracks = sheet.tracks().filter(|(_, audio)| *audio == path).collect();
            return Self::read_cue_tracks(&sheet, tracks, files);
        }

        let Some(read) = FileRead::read(path) else {
            return Vec::new();
        };
        let embedded = read
            .extra
            .cue_sheet
            .as_deref()
            .and_then(|text| SplitSheet::embedded(path, text));
        match embedded {
            Some(sheet) => {
                let cue_file = CueFile::new(&sheet.sheet, &read);
                sheet
                    .tracks()
                    .map(|(track, _)| Self::cue_track(&sheet, track, &cue_file))
                    .collect()
            }
            None => vec![Self::whole_file(file, &read)],
        }
    }

    fn read_cue_tracks(
        sheet: &SplitSheet,
        tracks: Vec<(&CueTrack, &Path)>,
        files: &FileCache,
    ) -> Vec<Self> {
        tracks
            .into_iter()
            .filter_map(|(track, audio)| {
                let cue_file = files.cue_file(sheet, audio)?;
                Some(Self::cue_track(sheet, track, &cue_file))
            })
            .collect()
    }

    // A track that is a whole file
    fn whole_file(file: gio::File, read: &FileRead) -> Self {
        let path = &read.path;
        let tagged_file = &read.tagged_file;
        let extra = &read.extra;

        let mut artist = None;
        let mut title = None;
        let mut album = None;
        let mut credits = TrackCredits::default();
        let mut tag_text = String::new();
        let mut cover: Option<AlbumCover> = None;
        let mut replay_gain = ReplayGain::default();
        if let Some(tag) = tagged_file.primary_tag() {
            debug!("Found primary tag");
            artist = track_artist(tag);
            title = track_title(tag);
            album = track_album(tag);
            credits = TrackCredits::from_tag(tag, extra);
            replay_gain = ReplayGain::from_tag(tag);
            tag_text = tag_search_text(tag);
            cover = Some(CoverCache::cover_art(path, tag));
        } else {
            warn!("Unable to load primary tag for: {path:?}");
            for tag in tagged_file.tags() {
                debug!("Found tag: {:?}", tag.tag_type());
                // The tags that come later fill in for the ones missing
                // from the first
                artist = artist.or_else(|| track_artist(tag));
                title = title.or_else(|| track_title(tag));
                album = album.or_else(|| track_album(tag));
                credits = TrackCredits::from_tag(tag, extra);
                replay_gain = ReplayGain::from_tag(tag);
                tag_text = tag_search_text(tag);
                // The first tag with a cover has the one we keep
                if cover.as_ref().is_none_or(|c| c.art().is_none()) {
                    cover = Some(CoverCache::cover_art(path, tag));
                }

                if artist.is_some() && title.is_some() {
                    break;
                }
            }
        }
        // A file without tags can still have a cover file next to it
        if cover.is_none() {
            let tag = Tag::new(tagged_file.primary_tag_type());
            cover = Some(CoverCache::cover_art(path, &tag));
        }

        let uuid = read
            .display_name
            .as_deref()
            .map(|name| track_uuid(name, None, [&artist, &title, &album]));
        let fallback_title = if title.is_none() {
            title_from_file_name(path)
        } else {
            String::new()
        };
        // The folder outside the sandbox, as a file that came through the
        // document portal is in a folder named after its document
        let fallback_album = if album.is_none() {
            fallback_album(read.host_path.as_deref().unwrap_or(path))
        } else {
            String::new()
        };

        TrackData {
            credits,
            tag_text,
            cover,
            uuid,
            duration: sane_duration(tagged_file.properties().duration()),
            details: read.details(replay_gain),
            file,
            audio: None,
            missing: false,
            artist,
            title,
            fallback_title,
            album,
            fallback_album,
        }
    }

    // A track of a CUE sheet, with the tags from the sheet over those of
    // its audio file
    fn cue_track(sheet: &SplitSheet, track: &CueTrack, cue_file: &CueFile) -> Self {
        let tag = cue_sheet::track_tag(&cue_file.tag, &sheet.sheet, track);
        // The other items are about the whole file
        let extra = ExtraComments::default();
        let artist = track_artist(&tag);
        let title = track_title(&tag);
        let album = track_album(&tag);

        let end = match track.end {
            Some(end) if cue_file.duration.is_zero() || end < cue_file.duration => end,
            _ => cue_file.duration,
        };
        let uuid = cue_file
            .display_name
            .as_deref()
            .map(|name| track_uuid(name, Some(track.number), [&artist, &title, &album]));
        let mut details = cue_file.details.clone();
        details.replay_gain = ReplayGain::from_tag(&tag);

        TrackData {
            credits: TrackCredits::from_tag(&tag, &extra),
            tag_text: tag_search_text(&tag),
            cover: Some(cue_file.cover.clone()),
            uuid,
            duration: sane_duration(end.saturating_sub(track.start)),
            details,
            file: gio::File::for_path(sheet.location(track)),
            audio: Some(CueAudio {
                file: gio::File::for_path(&cue_file.path),
                start: track.start,
                end: track.end,
                gains: Gains::from_tag(&tag),
            }),
            missing: false,
            artist,
            title,
            // Translators: the title of a track of a CUE sheet that has
            // none; `{}` is its number
            fallback_title: i18n_f("Track {}", &[&track.number.to_string()]),
            // The album is in the folder of the sheet
            fallback_album: if album.is_none() {
                fallback_album(&sheet.path)
            } else {
                String::new()
            },
            album,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absurd_lengths_count_as_unknown() {
        assert_eq!(sane_duration(Duration::from_secs(185)), 185);
        // An audiobook in a single file
        assert_eq!(sane_duration(Duration::from_secs(60 * 3600)), 60 * 3600);
        // A granule position of -1 at the end of an Ogg file, at 44.1 kHz
        assert_eq!(sane_duration(Duration::from_secs(u64::MAX / 44_100)), 0);
        assert_eq!(sane_duration(Duration::MAX), 0);
    }
}
