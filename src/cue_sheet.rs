// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! CUE sheets, which split an audio file into the tracks of a disc
//!
//! A sheet is a ".cue" file next to the audio file, or a "CUESHEET" tag of
//! the audio file. It names the audio files, and where each track starts
//! in its file, in frames of a CD, 75 a second; a track ends where the
//! next one starts in the same file, or with the file.
//!
//! A track of a sheet is at a path under the sheet, e.g.
//! "Album.cue/track03", or "Album.flac/track03" for the sheet in the tags
//! of "Album.flac". Nothing can be under a file, so the path is the
//! track's own: the queue, the playlists and the library keep it like the
//! path of a file.
//!
//! A sheet only splits the files it has several tracks in: the sheets of
//! rips with a file for each track repeat the tags of the files, which
//! often have more. Which sheet splits a file doesn't depend on how the
//! file was found: the first ".cue" in its folder that splits it, as the
//! folder lists them, or else the sheet in its tags.

use std::{
    borrow::Cow,
    cell::OnceCell,
    cmp::Ordering,
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};

use crate::utils;

// The frames of a CD, which the positions in a sheet count
const FRAMES_PER_SECOND: u64 = 75;

// Sheets are a few kilobytes; a larger file is something else
const MAX_SHEET_SIZE: u64 = 1024 * 1024;

// The format numbers the tracks of a CD up to 99, but sheets that split
// audiobooks into chapters can go on. Every track takes the tags of the
// whole file, so a sheet with any number of tracks could fill the memory
const MAX_TRACK_NUMBER: u32 = 999;

// CD-Text fields are at most 80 characters, so a longer value in a sheet
// is something else. Every track keeps a copy of the album's values, so
// they're cut there
const MAX_SHEET_VALUE: usize = 1024;

// lofty reads each item of a file up to its allocation limit, 16 MiB, and
// every track of a sheet takes the items of the audio file. All the tracks
// of a sheet together keep no more of them than lofty reads of one item,
// so a large item in a file split into many tracks can't fill the memory
const MAX_SHEET_TAG_TEXT: usize = 16 * 1024 * 1024;

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct CueSheet {
    pub(crate) performer: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) songwriter: Option<String>,
    // The UPC/EAN barcode of the disc
    pub(crate) catalog: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) date: Option<String>,
    pub(crate) composer: Option<String>,
    pub(crate) disc_number: Option<String>,
    pub(crate) disc_total: Option<String>,
    pub(crate) album_gain: Option<String>,
    pub(crate) album_peak: Option<String>,
    // The files, as the sheet names them
    pub(crate) files: Vec<String>,
    // The audio tracks, in order, each with a number of its own
    pub(crate) tracks: Vec<CueTrack>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct CueTrack {
    pub(crate) number: u32,
    // In `CueSheet::files`; `None` if no file came before the track
    pub(crate) file: Option<usize>,
    pub(crate) title: Option<String>,
    pub(crate) performer: Option<String>,
    pub(crate) songwriter: Option<String>,
    pub(crate) isrc: Option<String>,
    pub(crate) gain: Option<String>,
    pub(crate) peak: Option<String>,
    pub(crate) start: Duration,
    // Until the end of the file if `None`
    pub(crate) end: Option<Duration>,
}

impl CueTrack {
    // How long the track plays of an audio file `duration` long, which is
    // zero if it isn't known: up to the next track, or to the end of the file
    pub(crate) fn length(&self, duration: Duration) -> Duration {
        let end = match self.end {
            Some(end) if duration.is_zero() || end < duration => end,
            _ => duration,
        };
        end.saturating_sub(self.start)
    }
}

// A track as the sheet has it, including the tracks that are not audio,
// whose start still ends the track before them
#[derive(Default)]
struct SheetTrack {
    track: CueTrack,
    audio: bool,
    // Where index 01 is, or else the first index, and in which file
    start: Option<(Duration, Option<usize>)>,
    has_index_01: bool,
}

// The first word of a line, and the rest
fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (s, ""),
    }
}

// A value, which can be quoted or not; a quoted one ends at the last
// quote, as some sheets have quotes inside
fn unquote_whole(s: &str) -> Option<&str> {
    let s = s.trim();
    let value = match s.strip_prefix('"') {
        Some(rest) => rest.rfind('"').map_or(rest, |end| &rest[..end]),
        None => s,
    };
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

// A value, cut between characters where it's longer than MAX_SHEET_VALUE
fn unquote(s: &str) -> Option<String> {
    let value = unquote_whole(s)?;
    let value = value[..value.floor_char_boundary(MAX_SHEET_VALUE)].trim_end();
    (!value.is_empty()).then(|| value.to_string())
}

// The name in `FILE "name" TYPE`; unquoted, the type is the last word.
// The sheet keeps it once, not each track, so it's never cut
fn file_name(s: &str) -> Option<String> {
    let s = s.trim();
    let name = if s.starts_with('"') {
        unquote_whole(s)
    } else {
        match s.rsplit_once(char::is_whitespace) {
            Some((name, _)) => unquote_whole(name),
            None => unquote_whole(s),
        }
    };
    name.map(str::to_string)
}

// "mm:ss:ff"; the minutes can go past 99, but not past what a duration in
// nanoseconds holds
fn parse_time(s: &str) -> Option<Duration> {
    let mut parts = s.trim().split(':');
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    let frames: u64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || seconds >= 60 || frames >= FRAMES_PER_SECOND {
        return None;
    }
    let frames = minutes
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(FRAMES_PER_SECOND)?
        .checked_add(frames)?;
    Some(Duration::from_nanos(
        frames.checked_mul(1_000_000_000)? / FRAMES_PER_SECOND,
    ))
}

// The audio tracks of a sheet, if it has any
pub(crate) fn parse(text: &str) -> Option<CueSheet> {
    let mut sheet = CueSheet::default();
    let mut tracks: Vec<SheetTrack> = Vec::new();
    let mut file = None;

    for line in text.lines() {
        let (command, rest) = split_word(line);
        let command = command.to_ascii_uppercase();
        let track = tracks.last_mut();
        match (command.as_str(), track) {
            ("FILE", _) => {
                // GLib can't take a path with a NUL byte; the tracks of
                // such a file are left without one
                let name = file_name(rest).filter(|name| !name.contains('\0'));
                sheet.files.push(name.unwrap_or_default());
                file = Some(sheet.files.len() - 1);
            }
            ("TRACK", _) => {
                let (number, kind) = split_word(rest);
                tracks.push(SheetTrack {
                    track: CueTrack {
                        number: number.parse().unwrap_or(0),
                        file,
                        ..Default::default()
                    },
                    audio: kind.eq_ignore_ascii_case("AUDIO"),
                    ..Default::default()
                });
            }
            ("INDEX", Some(track)) => {
                let (number, time) = split_word(rest);
                let (Ok(number), Some(time)) = (number.parse::<u32>(), parse_time(time)) else {
                    continue;
                };
                if number == 1 {
                    track.start = Some((time, file));
                    track.has_index_01 = true;
                } else if track.start.is_none() {
                    track.start = Some((time, file));
                }
            }
            ("TITLE", Some(track)) => track.track.title = unquote(rest),
            ("TITLE", None) => sheet.title = unquote(rest),
            ("PERFORMER", Some(track)) => track.track.performer = unquote(rest),
            ("PERFORMER", None) => sheet.performer = unquote(rest),
            ("SONGWRITER", Some(track)) => track.track.songwriter = unquote(rest),
            ("SONGWRITER", None) => sheet.songwriter = unquote(rest),
            ("ISRC", Some(track)) => track.track.isrc = unquote(rest),
            ("CATALOG", None) => sheet.catalog = unquote(rest),
            ("REM", track) => {
                let (key, value) = split_word(rest);
                let value = unquote(value);
                match (key.to_ascii_uppercase().as_str(), track) {
                    ("REPLAYGAIN_TRACK_GAIN", Some(track)) => track.track.gain = value,
                    ("REPLAYGAIN_TRACK_PEAK", Some(track)) => track.track.peak = value,
                    ("GENRE", None) => sheet.genre = value,
                    ("DATE", None) => sheet.date = value,
                    ("COMPOSER", None) => sheet.composer = value,
                    ("DISCNUMBER", None) => sheet.disc_number = value,
                    ("TOTALDISCS", None) => sheet.disc_total = value,
                    ("REPLAYGAIN_ALBUM_GAIN", None) => sheet.album_gain = value,
                    ("REPLAYGAIN_ALBUM_PEAK", None) => sheet.album_peak = value,
                    _ => (),
                }
            }
            _ => (),
        }
    }

    // A track ends where the next one starts, if it's in the same file
    let starts: Vec<Option<(Duration, Option<usize>)>> = tracks.iter().map(|t| t.start).collect();
    let mut numbers = HashSet::new();
    for (i, mut track) in tracks.into_iter().enumerate() {
        let Some((start, file)) = track.start else {
            continue;
        };
        if !track.audio
            || !(1..=MAX_TRACK_NUMBER).contains(&track.track.number)
            || !numbers.insert(track.track.number)
        {
            continue;
        }
        // Index 01 is in the file where the track starts, after the gap
        // before it, which can be at the end of the file before
        if track.has_index_01 {
            track.track.file = file;
        }
        track.track.start = start;
        track.track.end = starts[i + 1..]
            .iter()
            .flatten()
            .next()
            .filter(|(_, next_file)| *next_file == track.track.file)
            .map(|(next_start, _)| *next_start)
            .filter(|end| *end > start);
        sheet.tracks.push(track.track);
    }

    (!sheet.tracks.is_empty()).then_some(sheet)
}

// Sheets are often in the legacy encoding of the system they were written
// on, e.g. Windows-1251 or Shift JIS, which is guessed from the text. Text
// in UTF-8 is the bytes themselves
pub(crate) fn decode(bytes: &[u8]) -> Cow<'_, str> {
    if let Some((encoding, bom_length)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode_without_bom_handling(&bytes[bom_length..]).0;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.into();
    }
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    detector
        .guess(None, Utf8Detection::Deny)
        .decode_without_bom_handling(bytes)
        .0
}

pub(crate) fn is_cue_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
}

fn read_sheet(path: &Path) -> Option<CueSheet> {
    let file = utils::open_regular_file(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_SHEET_SIZE).read_to_end(&mut bytes).ok()?;
    parse(&decode(&bytes))
}

// The path of a track of a sheet
pub(crate) fn track_location(sheet: &Path, number: u32) -> PathBuf {
    sheet.join(format!("track{number:02}"))
}

// The sheet and the number of a track of a sheet, if the path is one
pub(crate) fn parse_location(path: &Path) -> Option<(&Path, u32)> {
    track_parts(path).filter(|(sheet, _)| sheet.is_file())
}

// The sheet and the number a path would have as the track of a sheet that
// isn't there: a ".cue" file, or an audio file with a sheet in its tags,
// rather than a folder, which has a name without an extension
pub(crate) fn missing_location_parts(path: &Path) -> Option<(&Path, u32)> {
    track_parts(path).filter(|(sheet, _)| sheet.extension().is_some())
}

fn track_parts(path: &Path) -> Option<(&Path, u32)> {
    let digits = path.file_name()?.to_str()?.strip_prefix("track")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = digits.parse().ok()?;
    Some((path.parent()?, number))
}

// The file of a track: its sheet for a track of a sheet
pub(crate) fn real_path(path: &Path) -> &Path {
    parse_location(path).map_or(path, |(sheet, _)| sheet)
}

// Whether a track is still there, as a file or as a track of a sheet
pub(crate) fn location_exists(path: &Path) -> bool {
    path.is_file() || parse_location(path).is_some()
}

// The audio file of a track
pub(crate) fn audio_path(path: &Path) -> Option<PathBuf> {
    let Some((sheet, number)) = parse_location(path) else {
        return Some(path.to_path_buf());
    };
    if !is_cue_path(sheet) {
        return Some(sheet.to_path_buf());
    }
    let sheet = SplitSheet::read(sheet)?;
    let (_, audio) = sheet.track(number)?;
    Some(audio.to_path_buf())
}

fn is_audio_file(path: &Path) -> bool {
    utils::is_audio_path(path) && path.is_file()
}

// The audio files in a folder
fn audio_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_audio_file(path))
        .collect()
}

// The audio file that a sheet names. It's often not quite the one next to
// it: rippers name the WAV file they wrote, which was compressed after,
// e.g. "Album.wav" for "Album.flac", sometimes with the folder it was in on
// Windows, and the files are renamed without the sheet. The audio files in
// its folder are listed once for all the files a sheet names, in `listing`
fn resolve_file(
    sheet: &Path,
    name: &str,
    single_file: bool,
    listing: &OnceCell<Vec<PathBuf>>,
) -> Option<PathBuf> {
    let dir = sheet.parent()?;
    let exact = dir.join(name);
    if is_audio_file(&exact) {
        return Some(exact);
    }
    let base = name.rsplit(['\\', '/']).next()?;
    let in_dir = dir.join(base);
    if is_audio_file(&in_dir) {
        return Some(in_dir);
    }

    let files = listing.get_or_init(|| audio_files(dir));
    let stem = |path: &Path| path.file_stem().map(|s| s.to_string_lossy().to_lowercase());
    let named = stem(Path::new(base));
    let mut candidates: Vec<&PathBuf> = files
        .iter()
        .filter(|file| named.is_some() && stem(file) == named)
        .collect();
    if candidates.is_empty() && single_file {
        let sheet_stem = stem(sheet);
        candidates = files
            .iter()
            .filter(|file| stem(file) == sheet_stem)
            .collect();
    }
    candidates.sort();
    candidates.first().map(|file| (*file).clone())
}

// A sheet, with the audio files of the tracks it splits them into
#[derive(Debug)]
pub(crate) struct SplitSheet {
    // A ".cue" file, or the audio file with the sheet in its tags
    pub(crate) path: PathBuf,
    pub(crate) sheet: CueSheet,
    // The tracks of the files it splits, by their index in the sheet, with
    // their audio file
    tracks: Vec<(usize, PathBuf)>,
}

impl SplitSheet {
    // A ".cue" file
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let sheet = read_sheet(path)?;
        let single_file = sheet.files.len() == 1;
        // Only the files that tracks are in: a crafted sheet can name a
        // hundred thousand, and the library reads each sheet at each scan
        let used: HashSet<usize> = sheet.tracks.iter().filter_map(|track| track.file).collect();
        let listing = OnceCell::new();
        let audio: Vec<Option<PathBuf>> = sheet
            .files
            .iter()
            .enumerate()
            .map(|(i, name)| {
                used.contains(&i)
                    .then(|| resolve_file(path, name, single_file, &listing))
                    .flatten()
            })
            .collect();
        Self::split(path, sheet, |file| audio.get(file?)?.clone())
    }

    // The sheet in the tags of an audio file, which names the file as it
    // was before it was compressed or renamed
    pub(crate) fn embedded(path: &Path, text: &str) -> Option<Self> {
        let sheet = parse(text)?;
        Self::split(path, sheet, |file| {
            (file.unwrap_or(0) == 0).then(|| path.to_path_buf())
        })
    }

    fn split(
        path: &Path,
        sheet: CueSheet,
        audio_file: impl Fn(Option<usize>) -> Option<PathBuf>,
    ) -> Option<Self> {
        let tracks: Vec<(usize, PathBuf)> = sheet
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, track)| Some((i, audio_file(track.file)?)))
            .collect();
        let mut counts: HashMap<&Path, usize> = HashMap::new();
        for (_, audio) in &tracks {
            *counts.entry(audio).or_default() += 1;
        }
        let split: HashSet<PathBuf> = counts
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .map(|(audio, _)| audio.to_path_buf())
            .collect();
        let tracks: Vec<(usize, PathBuf)> = tracks
            .into_iter()
            .filter(|(_, audio)| split.contains(audio))
            .collect();

        (!tracks.is_empty()).then(|| Self {
            path: path.to_path_buf(),
            sheet,
            tracks,
        })
    }

    // The tracks of the files the sheet splits, with their audio file
    pub(crate) fn tracks(&self) -> impl Iterator<Item = (&CueTrack, &Path)> {
        self.tracks
            .iter()
            .map(|(i, audio)| (&self.sheet.tracks[*i], audio.as_path()))
    }

    pub(crate) fn track(&self, number: u32) -> Option<(&CueTrack, &Path)> {
        self.tracks().find(|(track, _)| track.number == number)
    }

    pub(crate) fn splits(&self, audio: &Path) -> bool {
        self.tracks.iter().any(|(_, file)| file == audio)
    }

    // The audio files it splits, in order
    pub(crate) fn audio_files(&self) -> Vec<&Path> {
        let mut files: Vec<&Path> = Vec::new();
        for (_, audio) in self.tracks() {
            if !files.contains(&audio) {
                files.push(audio);
            }
        }
        files
    }

    pub(crate) fn location(&self, track: &CueTrack) -> PathBuf {
        track_location(&self.path, track.number)
    }
}

// The order of the files in a folder, as `utils::load_files_from_folder()`
// lists them
fn cmp_names(a: &Path, b: &Path) -> Ordering {
    let name = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    utils::cmp_file_names(&name(a), &name(b))
}

// The ".cue" files in a folder that split audio files, in order
pub(crate) fn folder_sheets(dir: &Path) -> Vec<Arc<SplitSheet>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut cues: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_cue_path(path) && path.is_file())
        .collect();
    cues.sort_by(|a, b| cmp_names(a, b));
    cues.iter()
        .filter_map(|cue| SplitSheet::read(cue))
        .map(Arc::new)
        .collect()
}

// The ".cue" files among some files, and which one splits each audio file:
// the first one in the list that splits it
#[derive(Debug, Default)]
pub(crate) struct Claims {
    pub(crate) sheets: HashMap<PathBuf, Arc<SplitSheet>>,
    pub(crate) audio: HashMap<PathBuf, PathBuf>,
}

impl Claims {
    pub(crate) fn new<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Self {
        let mut claims = Self::default();
        for path in paths {
            if !is_cue_path(path) || claims.sheets.contains_key(path) {
                continue;
            }
            // A sheet that makes the reader panic is left out, rather than
            // keeping the others from splitting their files
            let Some(sheet) = utils::guarded(path.display(), || SplitSheet::read(path), || None)
            else {
                continue;
            };
            for audio in sheet.audio_files() {
                claims
                    .audio
                    .entry(audio.to_path_buf())
                    .or_insert_with(|| path.to_path_buf());
            }
            claims.sheets.insert(path.to_path_buf(), Arc::new(sheet));
        }
        claims
    }

    // The sheet that splits an audio file, if it's one of them
    pub(crate) fn sheet_of(&self, audio: &Path) -> Option<&Path> {
        self.audio.get(audio).map(PathBuf::as_path)
    }

    // The tracks of a sheet among them, for the files it splits first
    pub(crate) fn tracks_of(&self, sheet: &Path) -> Vec<(&CueTrack, &Path)> {
        let Some(split) = self.sheets.get(sheet) else {
            return Vec::new();
        };
        split
            .tracks()
            .filter(|(_, audio)| self.sheet_of(audio) == Some(sheet))
            .collect()
    }
}

// The items of an audio file that are about all of it, rather than about
// one of the tracks of the sheet that splits it, like the lyrics of the
// whole file. Its track gain stays for the tracks the sheet has none for:
// it's the one the file plays with
const FILE_ITEMS: [ItemKey; 8] = [
    ItemKey::TrackTitle,
    ItemKey::TrackNumber,
    ItemKey::TrackTotal,
    ItemKey::Isrc,
    ItemKey::MusicBrainzRecordingId,
    ItemKey::MusicBrainzTrackId,
    ItemKey::Lyrics,
    ItemKey::UnsyncLyrics,
];

fn set(tag: &mut Tag, key: ItemKey, value: Option<&String>) {
    if let Some(value) = value {
        tag.insert_text(key, value.clone());
    }
}

// Sets a name from the sheet. The file's sort names for the name it
// replaces would be another's, e.g. the album's composer for the track a
// SONGWRITER of its own wrote, so they go, unless the name is the same
fn set_name(tag: &mut Tag, key: ItemKey, sort_key: ItemKey, value: Option<&String>) {
    if let Some(value) = value {
        if !tag.get_strings(key).eq([value.as_str()]) {
            tag.remove_key(sort_key);
        }
        tag.insert_text(key, value.clone());
    }
}

// What an item of the file takes in each track that keeps it: the item
// itself, even with an empty value, and its text
fn item_size(item: &TagItem) -> usize {
    let value = match item.value() {
        ItemValue::Text(text) | ItemValue::Locator(text) => text.len(),
        ItemValue::Binary(bytes) => bytes.len(),
    };
    size_of::<TagItem>() + item.description().len() + value
}

// The items of an audio file that each of the `tracks` tracks of a sheet
// takes, in the file's order: all but those about the whole file, as far
// as a track's share of MAX_SHEET_TAG_TEXT goes. The smallest are kept
// first, so that a long comment is the one left out, rather than the
// artist and the album after it
fn album_items(file_tag: &Tag, tracks: usize) -> Vec<&TagItem> {
    let items: Vec<&TagItem> = file_tag
        .items()
        .filter(|item| !FILE_ITEMS.contains(&item.key()))
        .collect();
    let sizes: Vec<usize> = items.iter().map(|item| item_size(item)).collect();
    let mut smallest_first: Vec<usize> = (0..items.len()).collect();
    smallest_first.sort_by_key(|&i| sizes[i]);

    let share = MAX_SHEET_TAG_TEXT / tracks.max(1);
    let mut kept = vec![false; items.len()];
    let mut total: usize = 0;
    for i in smallest_first {
        total = total.saturating_add(sizes[i]);
        if total > share {
            break;
        }
        kept[i] = true;
    }
    items
        .into_iter()
        .zip(kept)
        .filter_map(|(item, kept)| kept.then_some(item))
        .collect()
}

// The tags of an album that a sheet splits from an audio file: the file's,
// with the album's from the sheet over them. The tag is a Vorbis comment,
// which takes any item, whatever the file has
pub(crate) fn album_tag(file_tag: Option<&Tag>, sheet: &CueSheet) -> Tag {
    let mut tag = Tag::new(TagType::VorbisComments);
    if let Some(file_tag) = file_tag {
        for item in album_items(file_tag, sheet.tracks.len()) {
            tag.push_unchecked(item.clone());
        }
        // The cover, read once for the album; the tracks don't take them
        for picture in file_tag.pictures() {
            tag.push_picture(picture.clone());
        }
    }

    set(&mut tag, ItemKey::AlbumTitle, sheet.title.as_ref());
    set_name(
        &mut tag,
        ItemKey::AlbumArtist,
        ItemKey::AlbumArtistSortOrder,
        sheet.performer.as_ref(),
    );
    set(&mut tag, ItemKey::Genre, sheet.genre.as_ref());
    set(&mut tag, ItemKey::RecordingDate, sheet.date.as_ref());
    set(&mut tag, ItemKey::DiscNumber, sheet.disc_number.as_ref());
    set(&mut tag, ItemKey::DiscTotal, sheet.disc_total.as_ref());
    set(&mut tag, ItemKey::Barcode, sheet.catalog.as_ref());
    set(
        &mut tag,
        ItemKey::ReplayGainAlbumGain,
        sheet.album_gain.as_ref(),
    );
    set(
        &mut tag,
        ItemKey::ReplayGainAlbumPeak,
        sheet.album_peak.as_ref(),
    );
    tag
}

// The tags of a track of a sheet, from those of its album, without the
// pictures, which the cover was read from
pub(crate) fn track_tag(album_tag: &Tag, sheet: &CueSheet, track: &CueTrack) -> Tag {
    let mut tag = Tag::new(TagType::VorbisComments);
    for item in album_tag.items() {
        tag.push_unchecked(item.clone());
    }

    set(&mut tag, ItemKey::TrackTitle, track.title.as_ref());
    set_name(
        &mut tag,
        ItemKey::TrackArtist,
        ItemKey::TrackArtistSortOrder,
        track.performer.as_ref().or(sheet.performer.as_ref()),
    );
    set_name(
        &mut tag,
        ItemKey::Composer,
        ItemKey::ComposerSortOrder,
        track
            .songwriter
            .as_ref()
            .or(sheet.songwriter.as_ref())
            .or(sheet.composer.as_ref()),
    );
    tag.insert_text(ItemKey::TrackNumber, track.number.to_string());
    tag.insert_text(ItemKey::TrackTotal, sheet.tracks.len().to_string());
    set(&mut tag, ItemKey::Isrc, track.isrc.as_ref());
    set(&mut tag, ItemKey::ReplayGainTrackGain, track.gain.as_ref());
    set(&mut tag, ItemKey::ReplayGainTrackPeak, track.peak.as_ref());
    tag
}

#[cfg(test)]
mod tests {
    use lofty::{
        prelude::Accessor,
        tag::{ItemValue, TagItem},
    };

    use super::*;

    const SHEET: &str = r#"REM GENRE "Classical"
REM DATE 1963
REM DISCID 8A0A750B
REM COMMENT "ExactAudioCopy v1.6"
REM REPLAYGAIN_ALBUM_GAIN -6.20 dB
CATALOG 5099909654523
PERFORMER "Philharmonia Orchestra"
TITLE "Elgar: Enigma Variations"
FILE "Elgar - Enigma Variations.wav" WAVE
  TRACK 01 AUDIO
    TITLE "Theme. Andante"
    PERFORMER "Sir John Barbirolli"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Variation I "C.A.E.""
    REM REPLAYGAIN_TRACK_GAIN -5.10 dB
    ISRC GBAYC6300001
    INDEX 00 01:31:40
    INDEX 01 01:33:00
  TRACK 03 AUDIO
    TITLE Variation II
    INDEX 01 03:20:74
"#;

    fn seconds(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn a_sheet_has_the_tracks_from_start_to_end() {
        let sheet = parse(SHEET).unwrap();
        assert_eq!(sheet.title.as_deref(), Some("Elgar: Enigma Variations"));
        assert_eq!(sheet.performer.as_deref(), Some("Philharmonia Orchestra"));
        assert_eq!(sheet.genre.as_deref(), Some("Classical"));
        assert_eq!(sheet.date.as_deref(), Some("1963"));
        assert_eq!(sheet.catalog.as_deref(), Some("5099909654523"));
        assert_eq!(sheet.album_gain.as_deref(), Some("-6.20 dB"));
        assert_eq!(sheet.files, vec!["Elgar - Enigma Variations.wav"]);

        let numbers: Vec<u32> = sheet.tracks.iter().map(|t| t.number).collect();
        assert_eq!(numbers, vec![1, 2, 3]);
        let [first, second, third] = &sheet.tracks[..] else {
            panic!();
        };
        assert_eq!(first.title.as_deref(), Some("Theme. Andante"));
        assert_eq!(first.performer.as_deref(), Some("Sir John Barbirolli"));
        assert_eq!(first.start, Duration::ZERO);
        // Up to index 01 of the next track, with the gap before it
        assert_eq!(first.end, Some(seconds(93.0)));
        // Up to the last quote
        assert_eq!(second.title.as_deref(), Some(r#"Variation I "C.A.E.""#));
        assert_eq!(second.isrc.as_deref(), Some("GBAYC6300001"));
        assert_eq!(second.gain.as_deref(), Some("-5.10 dB"));
        assert_eq!(second.start, seconds(93.0));
        assert_eq!(third.title.as_deref(), Some("Variation II"));
        assert_eq!(third.start.as_nanos(), (200 * 75 + 74) * 1_000_000_000 / 75);
        assert_eq!(third.end, None);
        assert_eq!(second.end, Some(third.start));
    }

    #[test]
    fn times_count_frames_and_can_pass_99_minutes() {
        assert_eq!(parse_time("00:00:00"), Some(Duration::ZERO));
        assert_eq!(parse_time("00:01:00"), Some(seconds(1.0)));
        assert_eq!(parse_time("00:00:15"), Some(Duration::from_millis(200)));
        assert_eq!(parse_time("120:00:00"), Some(seconds(7200.0)));
        assert_eq!(parse_time("00:60:00"), None);
        assert_eq!(parse_time("00:00:75"), None);
        assert_eq!(parse_time("1:2"), None);
        // Past what a duration in nanoseconds holds
        assert_eq!(parse_time("99999999999999:00:00"), None);
        assert_eq!(parse_time("18446744073709551615:59:74"), None);
    }

    #[test]
    fn file_names_with_nul_bytes_are_dropped() {
        let sheet = parse("FILE \"a\0b.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n").unwrap();
        assert_eq!(sheet.files, vec![""]);
    }

    #[test]
    fn track_numbers_stop_at_999() {
        let sheet = parse(
            "FILE \"book.flac\" WAVE\n\
             TRACK 998 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 999 AUDIO\nINDEX 01 01:00:00\n\
             TRACK 1000 AUDIO\nINDEX 01 02:00:00\n",
        )
        .unwrap();
        let numbers: Vec<u32> = sheet.tracks.iter().map(|t| t.number).collect();
        assert_eq!(numbers, vec![998, 999]);
    }

    #[test]
    fn data_tracks_still_end_the_track_before() {
        let sheet = parse(
            "FILE \"disc.bin\" BINARY\n\
             TRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 03:00:00\n\
             TRACK 03 MODE1/2352\nINDEX 01 05:00:00\n",
        )
        .unwrap();
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[1].end, Some(seconds(300.0)));
    }

    #[test]
    fn tracks_end_with_their_file() {
        // A rip with a file for each track, and the gap before a track at
        // the end of the file before
        let sheet = parse(
            "FILE \"01.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 00 04:10:00\n\
             FILE \"02.wav\" WAVE\nINDEX 01 00:00:00\n",
        )
        .unwrap();
        assert_eq!(sheet.tracks[0].file, Some(0));
        assert_eq!(sheet.tracks[0].end, None);
        assert_eq!(sheet.tracks[1].file, Some(1));
        assert_eq!(sheet.tracks[1].start, Duration::ZERO);
    }

    #[test]
    fn tracks_without_an_index_or_with_a_repeated_number_are_left_out() {
        let sheet = parse(
            "FILE a.flac WAVE\nTRACK 01 AUDIO\nTITLE One\nINDEX 01 00:00:00\n\
             TRACK 01 AUDIO\nTITLE Again\nINDEX 01 01:00:00\n\
             TRACK 02 AUDIO\nTITLE None\n",
        )
        .unwrap();
        let titles: Vec<_> = sheet.tracks.iter().map(|t| t.title.as_deref()).collect();
        assert_eq!(titles, vec![Some("One")]);
        // The unquoted name, without the type
        assert_eq!(sheet.files, vec!["a.flac"]);
        assert!(parse("REM nothing here\n").is_none());
    }

    #[test]
    fn sheets_in_legacy_encodings_are_decoded() {
        // "Кино" in Windows-1251
        let mut bytes = b"PERFORMER \"".to_vec();
        bytes.extend([0xCA, 0xE8, 0xED, 0xEE]);
        bytes.extend(b"\"\nTITLE \"\xC3\xF0\xF3\xEF\xEF\xE0 \xEA\xF0\xEE\xE2\xE8\"\n");
        assert!(decode(&bytes).contains("PERFORMER \"Кино\""));
        assert!(decode(&bytes).contains("Группа крови"));

        // UTF-16 with a byte order mark, as some Windows tools write them
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "TITLE \"Été\"".encode_utf16() {
            utf16.extend(unit.to_le_bytes());
        }
        assert_eq!(decode(&utf16), "TITLE \"Été\"");

        let utf8 = "\u{feff}TITLE \"Été\"".as_bytes();
        assert_eq!(decode(utf8), "TITLE \"Été\"");
    }

    #[test]
    fn locations_are_under_the_sheet() {
        let dir = std::env::temp_dir().join(format!("dismas-cue-location-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sheet = dir.join("Album.cue");
        std::fs::write(&sheet, SHEET).unwrap();

        let location = track_location(&sheet, 3);
        assert_eq!(location, dir.join("Album.cue/track03"));
        assert_eq!(parse_location(&location), Some((sheet.as_path(), 3)));
        assert_eq!(real_path(&location), sheet);
        assert!(location_exists(&location));
        assert!(!location_exists(&track_location(&dir.join("Gone.cue"), 1)));
        assert_eq!(parse_location(&dir.join("Album.cue/track")), None);
        assert_eq!(parse_location(&dir.join("Album.cue/track+3")), None);
        // A folder can have any name
        std::fs::create_dir_all(dir.join("Live.cue")).unwrap();
        assert_eq!(parse_location(&dir.join("Live.cue/track01")), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A crafted sheet can name a hundred thousand files, each of which was
    // looked for, with a listing of the folder, at each scan
    #[test]
    fn only_the_files_that_tracks_are_in_are_looked_for() {
        let dir = std::env::temp_dir().join(format!("dismas-cue-unused-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sheet = dir.join("Album.cue");
        let unused: String = (0..5000)
            .map(|i| format!("FILE \"Missing {i}.wav\" WAVE\n"))
            .collect();
        std::fs::write(
            &sheet,
            SHEET.replace("FILE \"Elgar", &format!("{unused}FILE \"Elgar")),
        )
        .unwrap();
        let flac = dir.join("Elgar - Enigma Variations.flac");
        std::fs::write(&flac, b"").unwrap();

        let split = SplitSheet::read(&sheet);
        std::fs::remove_dir_all(&dir).unwrap();
        let split = split.unwrap();
        assert_eq!(split.tracks().count(), 3);
        assert!(split.splits(&flac));
    }

    #[test]
    fn the_file_named_is_found_with_another_extension() {
        let dir = std::env::temp_dir().join(format!("dismas-cue-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sheet = dir.join("Album.cue");
        std::fs::write(&sheet, SHEET).unwrap();
        let flac = dir.join("Elgar - Enigma Variations.flac");
        std::fs::write(&flac, b"").unwrap();

        let resolve = |name, single_file| resolve_file(&sheet, name, single_file, &OnceCell::new());
        let name = "Elgar - Enigma Variations.wav";
        assert_eq!(resolve(name, true), Some(flac.clone()));
        let windows = r"C:\Rips\Elgar - Enigma Variations.wav";
        assert_eq!(resolve(windows, true), Some(flac.clone()));
        // Renamed, with the sheet's name
        std::fs::rename(&flac, dir.join("Album.flac")).unwrap();
        assert_eq!(resolve(name, true), Some(dir.join("Album.flac")));
        assert_eq!(resolve(name, false), None);

        let split = SplitSheet::read(&sheet).unwrap();
        assert_eq!(split.tracks().count(), 3);
        assert!(split.splits(&dir.join("Album.flac")));
        let location = track_location(&sheet, 2);
        assert_eq!(audio_path(&location), Some(dir.join("Album.flac")));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_files_with_several_tracks_are_split() {
        let sheet = "FILE \"01.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n\
                     FILE \"02.flac\" WAVE\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n";
        let path = Path::new("/music/Album.flac");
        assert!(SplitSheet::embedded(path, sheet).is_none());
        let split = SplitSheet::embedded(path, SHEET).unwrap();
        assert_eq!(split.audio_files(), vec![path]);
        assert_eq!(
            split.location(split.track(2).unwrap().0),
            Path::new("/music/Album.flac/track02")
        );
    }

    #[test]
    fn the_first_sheet_in_the_list_splits_a_file() {
        let dir = std::env::temp_dir().join(format!("dismas-cue-claims-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("Album.cue");
        let second = dir.join("Album (copy).cue");
        std::fs::write(&first, SHEET).unwrap();
        std::fs::write(&second, SHEET).unwrap();
        let audio = dir.join("Elgar - Enigma Variations.wav");
        std::fs::write(&audio, b"").unwrap();
        // Sorted as the folder lists them
        let sheets = folder_sheets(&dir);
        assert_eq!(sheets.len(), 2);

        let claims = Claims::new([first.as_path(), second.as_path(), audio.as_path()]);
        assert_eq!(claims.sheet_of(&audio), Some(first.as_path()));
        assert_eq!(claims.tracks_of(&first).len(), 3);
        assert!(claims.tracks_of(&second).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_tags_of_a_track_come_from_the_sheet_over_those_of_the_file() {
        let sheet = parse(SHEET).unwrap();
        let mut file_tag = Tag::new(TagType::Id3v2);
        file_tag.insert_text(ItemKey::TrackTitle, "Enigma Variations".to_string());
        file_tag.insert_text(ItemKey::AlbumTitle, "Elgar".to_string());
        file_tag.insert_text(ItemKey::Label, "EMI".to_string());
        file_tag.insert_text(ItemKey::TrackNumber, "1".to_string());
        file_tag.insert_text(ItemKey::ReplayGainTrackGain, "-6.00 dB".to_string());
        file_tag.insert_text(ItemKey::UnsyncLyrics, "Ave Maria".to_string());

        let album = album_tag(Some(&file_tag), &sheet);
        assert_eq!(album.album().as_deref(), Some("Elgar: Enigma Variations"));
        assert_eq!(album.title(), None);
        // The lyrics of the whole file aren't any track's
        assert_eq!(album.get_string(ItemKey::UnsyncLyrics), None);
        assert_eq!(album.get_string(ItemKey::Label), Some("EMI"));
        assert_eq!(album.get_string(ItemKey::RecordingDate), Some("1963"));
        assert_eq!(album.get_string(ItemKey::Barcode), Some("5099909654523"));

        let track = track_tag(&album, &sheet, &sheet.tracks[1]);
        assert_eq!(track.title().as_deref(), Some(r#"Variation I "C.A.E.""#));
        assert_eq!(track.artist().as_deref(), Some("Philharmonia Orchestra"));
        assert_eq!(
            track.get_string(ItemKey::AlbumArtist),
            Some("Philharmonia Orchestra")
        );
        assert_eq!(track.track(), Some(2));
        assert_eq!(track.track_total(), Some(3));
        assert_eq!(track.get_string(ItemKey::Isrc), Some("GBAYC6300001"));
        assert_eq!(
            track.get_string(ItemKey::ReplayGainTrackGain),
            Some("-5.10 dB")
        );
        let first = track_tag(&album, &sheet, &sheet.tracks[0]);
        assert_eq!(first.artist().as_deref(), Some("Sir John Barbirolli"));
        // Without a gain in the sheet, the file's, the whole disc's
        assert_eq!(
            first.get_string(ItemKey::ReplayGainTrackGain),
            Some("-6.00 dB")
        );
    }

    #[test]
    fn a_name_the_sheet_replaces_loses_its_sort_name() {
        let sheet = parse(
            r#"PERFORMER "Philharmonia Orchestra"
TITLE "English Music for Strings"
FILE "Strings.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Introduction and Allegro"
    SONGWRITER "Edward Elgar"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "An English Suite"
    SONGWRITER "Hubert Parry"
    INDEX 01 14:00:00
"#,
        )
        .unwrap();
        let mut file_tag = Tag::new(TagType::VorbisComments);
        for (key, value) in [
            (ItemKey::AlbumArtist, "Sir John Barbirolli"),
            (ItemKey::AlbumArtistSortOrder, "Barbirolli, Sir John"),
            (ItemKey::Composer, "Edward Elgar"),
            (ItemKey::ComposerSortOrder, "Elgar, Edward"),
        ] {
            file_tag.push_unchecked(TagItem::new(key, ItemValue::Text(value.to_string())));
        }

        // The sheet's performer isn't the file's album artist
        let album = album_tag(Some(&file_tag), &sheet);
        assert_eq!(
            album.get_string(ItemKey::AlbumArtist),
            Some("Philharmonia Orchestra")
        );
        assert_eq!(album.get_string(ItemKey::AlbumArtistSortOrder), None);

        // The first track's composer is the file's, the second's isn't
        let elgar = track_tag(&album, &sheet, &sheet.tracks[0]);
        assert_eq!(
            elgar.get_string(ItemKey::ComposerSortOrder),
            Some("Elgar, Edward")
        );
        let parry = track_tag(&album, &sheet, &sheet.tracks[1]);
        assert_eq!(parry.get_string(ItemKey::Composer), Some("Hubert Parry"));
        assert_eq!(parry.get_string(ItemKey::ComposerSortOrder), None);
    }

    #[test]
    fn long_values_are_cut_between_characters_but_file_names_are_not() {
        // The cut at 1024 bytes falls inside an "é", which starts at 1023
        let long = format!("a{}", "é".repeat(600));
        let quoted = format!("{}.flac", "b".repeat(2000));
        let unquoted = format!("{}.flac", "c".repeat(2000));
        let sheet = parse(&format!(
            "TITLE \"{long}\"\nREM COMPOSER {long}\n\
             FILE \"{quoted}\" WAVE\nTRACK 01 AUDIO\nTITLE {long}\nINDEX 01 00:00:00\n\
             FILE {unquoted} WAVE\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n"
        ))
        .unwrap();
        let cut = format!("a{}", "é".repeat(511));
        assert_eq!(sheet.title.as_deref(), Some(cut.as_str()));
        assert_eq!(sheet.composer.as_deref(), Some(cut.as_str()));
        assert_eq!(sheet.tracks[0].title.as_deref(), Some(cut.as_str()));
        assert_eq!(sheet.files, vec![quoted, unquoted]);
    }

    // The sheet of an audiobook with `count` chapters, which says nothing
    // about the album
    fn chapters(count: u32) -> CueSheet {
        let mut text = String::from("FILE \"book.flac\" WAVE\n");
        for number in 1..=count {
            text.push_str(&format!("TRACK {number} AUDIO\nINDEX 01 {number}:00:00\n"));
        }
        parse(&text).unwrap()
    }

    #[test]
    fn the_tracks_of_a_sheet_share_what_they_take_of_the_tags() {
        let mut file_tag = Tag::new(TagType::VorbisComments);
        for (key, value) in [
            (ItemKey::Comment, "x".repeat(64 * 1024)),
            (ItemKey::TrackArtist, "Kathleen Ferrier".to_string()),
            (ItemKey::AlbumTitle, "Blow the Wind Southerly".to_string()),
            (ItemKey::Label, "Decca".to_string()),
        ] {
            file_tag.push_unchecked(TagItem::new(key, ItemValue::Text(value)));
        }

        // A track's share of 999, about 16 KiB, can't take the comment,
        // which is left out rather than the smaller items after it
        let sheet = chapters(999);
        let album = album_tag(Some(&file_tag), &sheet);
        let keys: Vec<ItemKey> = album.items().map(TagItem::key).collect();
        assert_eq!(
            keys,
            [ItemKey::TrackArtist, ItemKey::AlbumTitle, ItemKey::Label]
        );
        let track = track_tag(&album, &sheet, &sheet.tracks[998]);
        assert_eq!(track.artist().as_deref(), Some("Kathleen Ferrier"));
        assert_eq!(track.album().as_deref(), Some("Blow the Wind Southerly"));
        assert_eq!(track.get_string(ItemKey::Comment), None);

        // A few tracks keep it, in the file's order
        let sheet = chapters(2);
        let album = album_tag(Some(&file_tag), &sheet);
        let keys: Vec<ItemKey> = album.items().map(TagItem::key).collect();
        assert_eq!(
            keys,
            [
                ItemKey::Comment,
                ItemKey::TrackArtist,
                ItemKey::AlbumTitle,
                ItemKey::Label
            ]
        );
    }

    #[test]
    fn empty_items_count_against_the_share_of_a_track() {
        let mut file_tag = Tag::new(TagType::VorbisComments);
        for _ in 0..100_000 {
            file_tag.push_unchecked(TagItem::new(
                ItemKey::CopyrightMessage,
                ItemValue::Text(String::new()),
            ));
        }

        let sheet = chapters(999);
        let album = album_tag(Some(&file_tag), &sheet);
        let track = track_tag(&album, &sheet, &sheet.tracks[0]);
        let copied = track
            .items()
            .filter(|item| item.key() == ItemKey::CopyrightMessage)
            .count();
        assert!(copied > 0);
        assert!(copied * size_of::<TagItem>() <= MAX_SHEET_TAG_TEXT / 999);
    }
}
