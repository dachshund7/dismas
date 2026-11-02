// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading the tracks of files, in a few threads

use super::*;

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
    tags: FileTags,
    display_name: Option<String>,
    size: Option<u64>,
    // The path outside the sandbox, if the file came through the document
    // portal
    host_path: Option<PathBuf>,
}

impl FileRead {
    fn read(path: &Path) -> Option<Self> {
        let tags = read_file_tags(path, ReadOptions::ALL)?;
        let info = gio::File::for_path(path)
            .query_info(
                &format!("standard::display-name,standard::size,{ATTRIBUTE_HOST_PATH}"),
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .ok();

        Some(Self {
            path: path.to_path_buf(),
            tags,
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
        let properties = &self.tags.properties;
        FileDetails {
            format: self.tags.format.clone(),
            path: self.host_path.clone().or_else(|| Some(self.path.clone())),
            size: self.size,
            bitrate: properties.audio_bitrate().filter(|b| *b > 0),
            sample_rate: properties.sample_rate().filter(|r| *r > 0),
            bit_depth: properties.bit_depth().filter(|d| *d > 0),
            channels: properties.channels().filter(|c| *c > 0),
            replay_gain,
            video: self.tags.video,
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
    // The properties of the whole file, which the length of each track
    // replaces
    properties: AudioProperties,
    display_name: Option<String>,
    details: FileDetails,
}

impl CueFile {
    fn new(sheet: &cue_sheet::CueSheet, read: FileRead) -> Self {
        let mut tag = cue_sheet::album_tag(Some(&read.tags.tag), sheet);
        // All the tracks have the same album, so they share the cover
        let cover = CoverCache::cover_art(&read.path, &tag);
        while !tag.pictures().is_empty() {
            tag.remove_picture(0);
        }
        let duration = read.tags.properties.duration();
        let properties = AudioProperties::of(&read.tags.properties);
        let details = read.details(ReplayGain::default());
        Self {
            path: read.path,
            tag,
            cover,
            duration,
            properties,
            display_name: read.display_name,
            details,
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
                read.tags.extra.cue_sheet.as_deref()?,
            )?);
            // Its tracks take the rest from the file that was just read
            let key = (path.to_path_buf(), path.to_path_buf());
            self.cue_files
                .get(key, || Some(Arc::new(CueFile::new(&sheet.sheet, read))));
            Some(sheet)
        })
    }

    fn cue_file(&self, sheet: &SplitSheet, audio: &Path) -> Option<Arc<CueFile>> {
        let key = (sheet.path.clone(), audio.to_path_buf());
        self.cue_files.get(key, || {
            let read = FileRead::read(audio)?;
            Some(Arc::new(CueFile::new(&sheet.sheet, read)))
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

// The track that keeps the place of a file of a playlist that gave none:
// it isn't there, or it is but couldn't be read, e.g. it's damaged, it
// can't be opened, or it isn't a regular file, like a named pipe. An audio
// file that a CUE sheet among the files splits has its tracks there, and
// needs none
fn placeholder(uri: &str, files: &FileCache) -> Option<TrackData> {
    let file = gio::File::for_uri(uri);
    let path = file.path()?;
    if files.claims.sheet_of(&path).is_some() {
        return None;
    }
    let reason = if cue_sheet::location_exists(&path) || path.exists() {
        Missing::Unreadable
    } else {
        Missing::NotFound
    };
    Some(TrackData::missing(file, reason))
}

// Reading the tags and decoding the covers takes a while, so the tracks
// are read in a few threads (`utils::in_threads`). They are sent in the
// order of the files, all the tracks of a file at once, which can be none;
// the threads stop once the receiver is dropped. With `placeholders`, for
// the tracks of a playlist, a file that gives no track is one that
// `placeholder()` makes, so that saving the playlist doesn't lose it
pub fn load_tracks(
    files: Vec<gio::File>,
    placeholders: bool,
) -> async_channel::Receiver<Vec<TrackData>> {
    let (sender, receiver) = async_channel::unbounded();

    let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
    let paths: Vec<PathBuf> = files.iter().filter_map(FileExt::path).collect();
    std::thread::spawn(move || {
        // Which sheets split the audio files, before any is read
        let cache = FileCache::new(Claims::new(paths.iter().map(PathBuf::as_path)));

        // The tracks read out of order wait for the ones before them
        let mut waiting = BTreeMap::new();
        let mut next_index = 0;
        let read = |uri: &String| {
            // A file whose tags make the reader panic is skipped, rather
            // than leaving the tracks after it unread
            let mut data = utils::guarded(uri, || TrackData::read(uri, &cache), Vec::new);
            if data.is_empty()
                && placeholders
                && let Some(placeholder) = placeholder(uri, &cache)
            {
                data.push(placeholder);
            }
            data
        };
        // Stops once the receiver is gone
        let _ = utils::in_threads(&uris, utils::READING_THREADS, read, |index, data| {
            waiting.insert(index, data);
            while let Some(data) = waiting.remove(&next_index) {
                sender.try_send(data).map_err(drop)?;
                next_index += 1;
            }
            Ok::<_, ()>(())
        });
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
        if !utils::is_audio_path(path) {
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
            .tags
            .extra
            .cue_sheet
            .as_deref()
            .and_then(|text| SplitSheet::embedded(path, text));
        match embedded {
            Some(sheet) => {
                let cue_file = CueFile::new(&sheet.sheet, read);
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
        let tag = &read.tags.tag;
        let extra = &read.tags.extra;
        let artist = track_artist(tag);
        let title = track_title(tag);
        let album = track_album(tag);
        let replay_gain = ReplayGain::from_tags(tag, extra);

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
        let folder = read.host_path.as_deref().unwrap_or(path);
        let fallback_album = if album.is_none() {
            fallback_album(folder)
        } else {
            String::new()
        };
        // As the library names it, without the "Unknown album" shown
        let folder_album = album
            .is_none()
            .then(|| folder_album_title(folder))
            .flatten();

        let credits = TrackCredits::from_tag(tag, extra);
        let fields = FieldParts {
            tag,
            credits: &credits,
            title: title.as_deref().or(Some(&fallback_title)),
            album: album.as_deref().or(folder_album.as_deref()),
            format: read.tags.format.as_deref(),
            properties: Some(AudioProperties::of(&read.tags.properties)),
        }
        .encode();
        TrackData {
            tag_text: track_search_text(tag, &credits),
            fields,
            credits,
            cover: Some(CoverCache::cover_art(path, tag)),
            uuid,
            duration: sane_duration(read.tags.properties.duration()),
            details: read.details(replay_gain),
            file,
            audio: None,
            missing: None,
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

        let length = track.length(cue_file.duration);
        let uuid = cue_file
            .display_name
            .as_deref()
            .map(|name| track_uuid(name, Some(track.number), [&artist, &title, &album]));
        let mut details = cue_file.details.clone();
        details.replay_gain = ReplayGain::from_tag(&tag);

        let credits = TrackCredits::from_tag(&tag, &extra);
        // The album is in the folder of the sheet
        let folder_album = album
            .is_none()
            .then(|| folder_album_title(&sheet.path))
            .flatten();
        let fields = FieldParts {
            tag: &tag,
            credits: &credits,
            title: title.as_deref(),
            album: album.as_deref().or(folder_album.as_deref()),
            format: cue_file.details.format.as_deref(),
            properties: Some(cue_file.properties.with_length(length)),
        }
        .encode();
        TrackData {
            tag_text: track_search_text(&tag, &credits),
            fields,
            credits,
            cover: Some(cue_file.cover.clone()),
            uuid,
            duration: sane_duration(length),
            details,
            file: gio::File::for_path(sheet.location(track)),
            audio: Some(CueAudio {
                file: gio::File::for_path(&cue_file.path),
                start: track.start,
                end: track.end,
                gains: Gains::from_tag(&tag),
            }),
            missing: None,
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

    // What `load_tracks` reads from the files, in their order
    fn load(files: &[&Path], placeholders: bool) -> Vec<Vec<TrackData>> {
        let files = files.iter().map(gio::File::for_path).collect();
        let receiver = load_tracks(files, placeholders);
        std::iter::from_fn(|| receiver.recv_blocking().ok()).collect()
    }

    #[test]
    fn files_of_a_playlist_that_give_no_tracks_keep_their_places() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-placeholders-XXXXXX")).unwrap();
        let damaged = dir.join("Damaged.flac");
        std::fs::write(&damaged, "not audio").unwrap();
        let gone = dir.join("Gone.flac");
        let wav = dir.join("Album.wav");
        crate::utils::write_test_wav(&wav, 4);
        let cue = dir.join("Album.cue");
        std::fs::write(
            &cue,
            "FILE \"Album.wav\" WAVE\n\
             TRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 00:02:00\n",
        )
        .unwrap();

        let read = load(&[&damaged, &gone, &cue, &wav, &cue.join("track07")], true);
        let reasons: Vec<Vec<Option<Missing>>> = read
            .iter()
            .map(|tracks| tracks.iter().map(TrackData::missing_reason).collect())
            .collect();
        assert_eq!(
            reasons,
            [
                vec![Some(Missing::Unreadable)],
                vec![Some(Missing::NotFound)],
                // The sheet has the tracks of the file it splits, which has
                // none, and keeps no place of its own
                vec![None, None],
                vec![],
                // A track the sheet doesn't have
                vec![Some(Missing::Unreadable)],
            ]
        );
        assert_eq!(read[0][0].shown_title(), "Damaged");

        // Files that are added to a playlist, rather than its own, are left
        // out
        let read = load(&[&damaged, &gone], false);
        assert!(read.iter().all(Vec::is_empty));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_named_pipe_in_a_playlist_keeps_its_place_without_stopping_the_load() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-placeholders-XXXXXX")).unwrap();
        let pipe = dir.join("Pipe.flac");
        crate::utils::make_test_pipe(&pipe);
        let wav = dir.join("Track.wav");
        crate::utils::write_test_wav(&wav, 1);

        let files = [pipe, wav];
        let reasons = crate::utils::within_seconds(move || {
            let files: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
            load(&files, true)
                .iter()
                .map(|tracks| tracks.iter().map(TrackData::missing_reason).collect())
                .collect::<Vec<Vec<Option<Missing>>>>()
        });
        assert_eq!(reasons, [vec![Some(Missing::Unreadable)], vec![None]]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

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
