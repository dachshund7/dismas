// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Lyrics, as the tags and the files next to the tracks give them.
//! Synchronized lyrics, in the LRC format, have the time each line is sung
//! at before it, e.g. "[01:23.45]", can have the time of each word in the
//! line, "<01:23.45>", and start with header lines, like
//! "[ar:Franz Schubert]"

use std::{
    borrow::Cow,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use lofty::tag::ItemKey;
use log::debug;

use crate::{
    audio::{ReadOptions, read_file_tags},
    cue_sheet, utils,
};

// The largest file of lyrics that's read, as lyrics take a few kilobytes
const MAX_LYRICS_FILE_SIZE: u64 = 1024 * 1024;

// The most that the lines of synchronized lyrics are copied into, in
// bytes: a line sung several times is copied for each time, and lyrics
// whose copies come to more than this aren't lyrics
const MAX_SYNCED_TEXT: usize = 4 * 1024 * 1024;

// The most lines synchronized lyrics have, far more than a song has: each
// is a row of a list, and a crafted file of a hundred thousand lines took
// the window seconds to show, and gigabytes; it shows as plain lyrics
const MAX_SYNCED_LINES: usize = 5000;

// The length of the longest time `timestamp()` takes, "99999:59.999"
const MAX_TIMESTAMP_LEN: usize = 12;

// The extensions of the files of lyrics next to an audio file, with the
// same name: LRC files, which lyrics downloaders write, then text files,
// which can be in the LRC format too
const LYRICS_EXTENSIONS: [&str; 2] = ["lrc", "txt"];

// The header lines of LRC files: the artist, the album, the title, the
// author, the lyricist, the maker of the file, the length of the track,
// the offset of the times, the program that made the file and its
// version, and comments
const HEADER_TAGS: [&str; 12] = [
    "ar", "al", "ti", "au", "lr", "by", "length", "offset", "re", "tool", "ve", "#",
];

// The lyrics of a track
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lyrics {
    // Lines of text, without times
    Plain(String),
    // The lines in the order they're sung
    Synced(Vec<Line>),
}

// A line of synchronized lyrics: when it's sung, from the start of the
// track, until when if a pause follows it, and its text, which can be on
// more than one line, e.g. with its translation
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) time: Duration,
    pub(crate) end: Option<Duration>,
    pub(crate) text: String,
}

// The lyrics of an audio file: the first synchronized ones of the files of
// lyrics next to it, of an ID3v2 SYLT frame and of the lyrics in its tags,
// or else the first plain ones. A track of a CUE sheet plays a `part` of
// its file, and gets the lines sung in it, see `in_part()`. The files are
// read here, so this doesn't run on the main thread
pub(crate) fn find(
    audio_file: &Path,
    part: Option<(Duration, Option<Duration>)>,
) -> Option<Lyrics> {
    let mut plain = None;
    for text in lyrics_file_texts(audio_file) {
        match parse(&text) {
            Some(lyrics @ Lyrics::Synced(_)) => return in_part(lyrics, part),
            lyrics => plain = plain.or(lyrics),
        }
    }
    if let Some(tags) = read_file_tags(audio_file, ReadOptions::LYRICS) {
        // Like the lyrics in the tags below, not when they'd make a larger
        // file than one of lyrics that's read, counting a byte for each line
        let sylt = tags.extra.synced_lyrics.filter(|entries| {
            entries
                .iter()
                .map(|(_, text)| text.len() as u64 + 1)
                .sum::<u64>()
                <= MAX_LYRICS_FILE_SIZE
        });
        if let Some(lyrics) = sylt.and_then(sylt_lyrics) {
            return in_part(lyrics, part);
        }
        // Lyrics in the tags larger than a file of lyrics that's read
        // aren't read either
        let text = [ItemKey::Lyrics, ItemKey::UnsyncLyrics]
            .into_iter()
            .filter_map(|key| tags.tag.get_string(key))
            .filter(|text| text.len() as u64 <= MAX_LYRICS_FILE_SIZE)
            .find(|text| !text.trim().is_empty());
        match text.and_then(parse) {
            Some(lyrics @ Lyrics::Synced(_)) => return in_part(lyrics, part),
            lyrics => plain = plain.or(lyrics),
        }
    }
    in_part(plain?, part)
}

// The lyrics of a part of a file, from its start: the lines of
// synchronized lyrics sung in it. Plain lyrics are the whole file's, so
// a part gets none
fn in_part(lyrics: Lyrics, part: Option<(Duration, Option<Duration>)>) -> Option<Lyrics> {
    let Some((start, end)) = part else {
        return Some(lyrics);
    };
    let Lyrics::Synced(lines) = lyrics else {
        return None;
    };
    let lines: Vec<Line> = lines
        .into_iter()
        .filter(|line| line.time >= start && end.is_none_or(|end| line.time < end))
        .map(|line| Line {
            time: line.time - start,
            end: line.end.map(|end| end.saturating_sub(start)),
            text: line.text,
        })
        .collect();
    (!lines.is_empty()).then_some(Lyrics::Synced(lines))
}

// Where the files of lyrics of an audio file are: next to it, with the
// same name, e.g. "Die Forelle.lrc" and "Die Forelle.txt" for
// "Die Forelle.flac"
pub(crate) fn lyrics_file_paths(audio_file: &Path) -> [PathBuf; 2] {
    LYRICS_EXTENSIONS.map(|extension| audio_file.with_extension(extension))
}

// The texts of the files of lyrics of an audio file, as they're needed:
// for each extension, the file with it, or else with it in capitals, as
// programs on Windows write it
fn lyrics_file_texts(audio_file: &Path) -> impl Iterator<Item = String> {
    LYRICS_EXTENSIONS.into_iter().filter_map(move |extension| {
        [extension.to_string(), extension.to_uppercase()]
            .iter()
            .find_map(|extension| read_text_file(&audio_file.with_extension(extension)))
    })
}

// The text of a file, unless it's too large to be lyrics
fn read_text_file(path: &Path) -> Option<String> {
    // Not a named pipe, which would wait for something to write to it
    let file = utils::open_regular_file(path).ok()?;
    if file.metadata().ok()?.len() > MAX_LYRICS_FILE_SIZE {
        debug!("Not reading {path:?}, which is too large to be lyrics");
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_LYRICS_FILE_SIZE)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(cue_sheet::decode(&bytes).into_owned())
}

// The lyrics of a text, synchronized if it's in the LRC format: when at
// least as many of its lines of text have a time as don't. A line without
// a time right after one with a time goes on from it, as some files put a
// line's translation on the next line
fn parse(text: &str) -> Option<Lyrics> {
    // In milliseconds; a positive offset shows the lines earlier
    let mut offset = 0i64;
    let mut entries: Vec<(Duration, String)> = Vec::new();
    // The entries of the last line with times
    let mut last = 0..0;
    let (mut timed, mut untimed) = (0, 0);
    // What the entries take so far, in bytes
    let mut copied = 0;
    for line in text.lines() {
        if let Some((tag, value)) = header(line) {
            if tag.eq_ignore_ascii_case("offset") {
                offset = value.parse().unwrap_or(0);
            }
            continue;
        }
        let (times, rest) = leading_times(line);
        let words = without_word_times(rest);
        let line_text = words.trim();
        if times.is_empty() {
            if line_text.is_empty() {
                last = 0..0;
                continue;
            }
            untimed += 1;
            if too_much_to_copy(&mut copied, last.len(), line_text.len() + 1) {
                return plain(text);
            }
            for (_, text) in &mut entries[last.clone()] {
                text.push('\n');
                text.push_str(line_text);
            }
            continue;
        }
        let entry_size = size_of::<(Duration, String)>() + line_text.len();
        if too_much_to_copy(&mut copied, times.len(), entry_size) {
            return plain(text);
        }
        let start = entries.len();
        entries.extend(times.into_iter().map(|time| (time, line_text.to_string())));
        last = if line_text.is_empty() {
            0..0
        } else {
            timed += 1;
            start..entries.len()
        };
    }
    if timed == 0 || timed < untimed {
        return plain(text);
    }
    let entries = entries
        .into_iter()
        .map(|(time, text)| (shifted(time, offset), text))
        .collect();
    synced(entries).or_else(|| plain(text))
}

// Adds `copies` of `size` bytes to what's `copied` into the entries of
// synchronized lyrics, and says whether that comes to more than they take
fn too_much_to_copy(copied: &mut usize, copies: usize, size: usize) -> bool {
    *copied = copied.saturating_add(copies.saturating_mul(size));
    *copied > MAX_SYNCED_TEXT
}

fn shifted(time: Duration, offset: i64) -> Duration {
    let by = Duration::from_millis(offset.unsigned_abs());
    if offset > 0 {
        time.saturating_sub(by)
    } else {
        time.saturating_add(by)
    }
}

// Lyrics as plain text, without the times of synchronized lyrics, as when
// too few of their lines have one
fn plain(text: &str) -> Option<Lyrics> {
    // Windows and old Macs end lines in their own ways, which a copy of
    // the text would keep
    let text = text_without_times(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let text = text.trim();
    (!text.is_empty()).then(|| Lyrics::Plain(text.to_string()))
}

// Synchronized lyrics from the time and the text of each line, in any
// order: lines sung at the same time are one, e.g. a line and its
// translation, and an empty line ends the one before it
fn synced(mut entries: Vec<(Duration, String)>) -> Option<Lyrics> {
    // A stable sort, which keeps a line before its translation
    entries.sort_by_key(|(time, _)| *time);
    let mut lines: Vec<Line> = Vec::new();
    for (time, text) in entries {
        let text = text.trim();
        let last = lines.last_mut();
        if text.is_empty() {
            if let Some(last) = last
                && last.end.is_none()
                && last.time < time
            {
                last.end = Some(time);
            }
        } else if let Some(last) = last.filter(|last| last.time == time) {
            last.text.push('\n');
            last.text.push_str(text);
        } else {
            if lines.len() == MAX_SYNCED_LINES {
                return None;
            }
            lines.push(Line {
                time,
                end: None,
                text: text.to_string(),
            });
        }
    }
    (!lines.is_empty()).then_some(Lyrics::Synced(lines))
}

// The lines of the entries of an ID3v2 SYLT frame, each a line or a word:
// when some of them start with a line break, those start the lines, and
// each of the others goes on from the one before
fn sylt_lyrics(entries: Vec<(Duration, String)>) -> Option<Lyrics> {
    let by_words = entries
        .iter()
        .any(|(_, text)| text.starts_with(['\n', '\r']));
    let mut lines: Vec<(Duration, String)> = Vec::new();
    for (time, text) in entries {
        let starts_line = text.starts_with(['\n', '\r']);
        let text = text.trim_start_matches(['\r', '\n']);
        match lines.last_mut() {
            Some((_, line)) if by_words && !starts_line => line.push_str(text),
            _ => lines.push((time, text.to_string())),
        }
    }
    synced(lines)
}

// Where a position is in synchronized lyrics. No line is sung either
// before the first line or in a pause, but the lines show from the top
// before the first one, and stay where they are in a pause
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Place {
    // Before the first line, as when the track starts, or in lyrics
    // without lines
    #[default]
    Start,
    // While a line is sung
    Line(usize),
    // After a line that has ended, the last one included
    Pause,
}

impl Place {
    // The line sung, if any
    pub(crate) fn line(self) -> Option<usize> {
        match self {
            Self::Line(line) => Some(line),
            Self::Start | Self::Pause => None,
        }
    }
}

// Where a position is: in the last line to start by then, unless it has
// ended
pub(crate) fn place_at(lines: &[Line], position: Duration) -> Place {
    let Some(line) = lines
        .partition_point(|line| line.time <= position)
        .checked_sub(1)
    else {
        return Place::Start;
    };
    if lines[line].end.is_none_or(|end| position < end) {
        Place::Line(line)
    } else {
        Place::Pause
    }
}

// When the line sung changes next after a position: the next line starts,
// or the one sung ends
pub(crate) fn next_change(lines: &[Line], position: Duration) -> Option<Duration> {
    let next = lines.partition_point(|line| line.time <= position);
    let start = lines.get(next).map(|line| line.time);
    let end = next
        .checked_sub(1)
        .and_then(|line| lines[line].end)
        .filter(|end| *end > position);
    match (start, end) {
        (Some(start), Some(end)) => Some(start.min(end)),
        (start, end) => start.or(end),
    }
}

// The text of lyrics without the times of synchronized lyrics and without
// their header lines, e.g. for searching, where "[01:12.50]" would match
// a search for "12". Other text in brackets, like "[Chorus]", stays
pub(crate) fn text_without_times(text: &str) -> Cow<'_, str> {
    if !text.contains(['[', '<']) {
        return Cow::Borrowed(text);
    }
    let mut lines = String::with_capacity(text.len());
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if header(content).is_some() {
            changed = true;
            continue;
        }
        // Times are only ever taken out, so the same length is the same
        // line
        let stripped = line_without_times(content);
        if stripped.len() == content.len() {
            lines.push_str(line);
        } else {
            changed = true;
            lines.push_str(&stripped);
            lines.push_str(&line[content.len()..]);
        }
    }
    if changed {
        Cow::Owned(lines)
    } else {
        Cow::Borrowed(text)
    }
}

// The tag and the value of a header line of an LRC file, like
// "[ar:Franz Schubert]"
fn header(line: &str) -> Option<(&str, &str)> {
    let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (tag, value) = inner.split_once(':')?;
    let tag = tag.trim();
    HEADER_TAGS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(tag))
        .then_some((tag, value.trim()))
}

fn line_without_times(line: &str) -> Cow<'_, str> {
    let (_, text) = leading_times(line);
    if text.contains('<') {
        without_word_times(text)
    } else {
        Cow::Borrowed(text)
    }
}

// The times a line is sung at, of which there can be several when the line
// comes back, e.g. "[00:12.00][01:05.50]Refrain", and its text
fn leading_times(line: &str) -> (Vec<Duration>, &str) {
    let mut text = line.trim_start();
    let mut times = Vec::new();
    while let Some((time, after)) = bracketed(text, '[', ']')
        && let Some(time) = timestamp(time)
    {
        times.push(time);
        text = after;
    }
    // A space after the times only sets the text apart from them
    if times.is_empty() {
        (times, line)
    } else {
        (times, text.trim_start())
    }
}

// The text of a line without the times of its words, as in
// "<00:12.00>In <00:12.40>einem <00:12.80>Bächlein"
fn without_word_times(line: &str) -> Cow<'_, str> {
    let mut text = String::with_capacity(line.len());
    let mut rest = line;
    let mut changed = false;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        match bracketed(&rest[start..], '<', '>') {
            Some((time, after)) if timestamp(time).is_some() => {
                changed = true;
                // With a space on both sides of the time, the text would
                // have two
                rest = if text.is_empty() || text.ends_with(' ') {
                    after.trim_start_matches(' ')
                } else {
                    after
                };
            }
            _ => {
                text.push('<');
                rest = &rest[start + 1..];
            }
        }
    }
    if !changed {
        return Cow::Borrowed(line);
    }
    text.push_str(rest);
    text.truncate(text.trim_end().len());
    Cow::Owned(text)
}

// The text of a time between `open`, at the start of `text`, and the next
// `close`, and what follows. A `close` further on than the longest time
// isn't looked for, so a line with many an `open` and no `close` takes no
// longer than its length
fn bracketed(text: &str, open: char, close: char) -> Option<(&str, &str)> {
    let inner = text.strip_prefix(open)?;
    let (end, _) = inner
        .char_indices()
        .take(MAX_TIMESTAMP_LEN + 1)
        .find(|&(_, c)| c == close)?;
    Some((&inner[..end], &inner[end + close.len_utf8()..]))
}

// The time of an LRC timestamp, without its brackets: minutes, any number
// of them, and seconds, with a fraction or not, e.g. "1:05", "01:05.5",
// "01:05.50", "01:05.500", or "01:05:50", with hundredths after a colon
fn timestamp(text: &str) -> Option<Duration> {
    let (minutes, rest) = text.split_once(':')?;
    let (seconds, fraction) = match rest.find(['.', ':']) {
        Some(i) => (&rest[..i], Some((rest.as_bytes()[i], &rest[i + 1..]))),
        None => (rest, None),
    };
    let minutes = number(minutes, 5)?;
    let seconds = number(seconds, 2)?;
    if seconds >= 60 {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some((b'.', digits)) => number(digits, 3)? * 10u64.pow(3 - digits.len() as u32),
        Some((_, digits)) if digits.len() == 2 => number(digits, 2)? * 10,
        Some(_) => return None,
    };
    Some(Duration::from_millis(
        (minutes * 60 + seconds) * 1000 + millis,
    ))
}

// A number of at most `max_digits` digits, and nothing else
fn number(text: &str, max_digits: usize) -> Option<u64> {
    if text.is_empty() || text.len() > max_digits || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use gtk::glib;
    use lofty::{
        id3::v2::{BinaryFrame, Frame, FrameId, Id3v2Tag},
        tag::{Tag, TagType},
    };

    use super::*;

    // A WAV file whose ID3v2 tag has the given frames
    fn tagged_wav(path: &Path, plain: Option<&str>, sylt: Option<Frame<'static>>) {
        crate::utils::write_test_wav(path, 1);
        let mut tag = Tag::new(TagType::Id3v2);
        if let Some(plain) = plain {
            tag.insert_text(ItemKey::UnsyncLyrics, plain.to_string());
        }
        let mut id3v2 = Id3v2Tag::from(tag);
        if let Some(sylt) = sylt {
            id3v2.insert(sylt);
        }
        crate::utils::write_test_id3v2(path, &id3v2, false);
    }

    // A SYLT frame of lyrics, with times in milliseconds, built byte by
    // byte, as lofty only writes frames with its write options
    fn sylt_frame(entries: &[(&str, u32)]) -> Frame<'static> {
        // UTF-8, the language, milliseconds, lyrics, and no description
        let mut data = vec![3];
        data.extend_from_slice(b"deu");
        data.extend_from_slice(&[2, 1, 0]);
        for (text, ms) in entries {
            data.extend_from_slice(text.as_bytes());
            data.push(0);
            data.extend_from_slice(&ms.to_be_bytes());
        }
        Frame::Binary(BinaryFrame::new(
            FrameId::Valid(Cow::Borrowed("SYLT")),
            data,
        ))
    }

    fn line(ms: u64, end: Option<u64>, text: &str) -> Line {
        Line {
            time: Duration::from_millis(ms),
            end: end.map(Duration::from_millis),
            text: text.to_string(),
        }
    }

    #[test]
    fn synchronized_lyrics_come_before_plain_ones() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-lyrics-XXXXXX")).unwrap();
        let audio = dir.join("Die Forelle.wav");
        tagged_wav(&audio, Some("Ave Maria"), None);
        let from_tags = find(&audio, None);

        // The LRC file's, when they're plain too
        std::fs::write(audio.with_extension("lrc"), "Ave Maria,\ngratia plena").unwrap();
        let from_plain_file = find(&audio, None);

        // In Latin-1, as some programs on Windows write them
        std::fs::write(
            audio.with_extension("lrc"),
            b"[ti:Die Forelle]\r\n[00:12.00]In einem B\xe4chlein helle,\r\n[00:16.50]Da scho\xdf in froher Eil\r\n",
        )
        .unwrap();
        let from_file = find(&audio, None);
        // The parts of a file that the tracks of a CUE sheet play
        let part = find(&audio, Some((Duration::from_secs(10), None)));
        let later_part = find(&audio, Some((Duration::from_secs(30), None)));
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(from_tags, Some(Lyrics::Plain("Ave Maria".to_string())));
        assert_eq!(
            from_plain_file,
            Some(Lyrics::Plain("Ave Maria,\ngratia plena".to_string()))
        );
        assert_eq!(
            from_file,
            Some(Lyrics::Synced(vec![
                line(12_000, None, "In einem Bächlein helle,"),
                line(16_500, None, "Da schoß in froher Eil"),
            ]))
        );
        assert_eq!(
            part,
            Some(Lyrics::Synced(vec![
                line(2_000, None, "In einem Bächlein helle,"),
                line(6_500, None, "Da schoß in froher Eil"),
            ]))
        );
        assert_eq!(later_part, None);
    }

    #[test]
    fn text_files_are_read_after_lrc_files() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-lyrics-XXXXXX")).unwrap();
        let audio = dir.join("Die Forelle.wav");
        tagged_wav(&audio, None, None);
        let (lrc, txt) = (audio.with_extension("lrc"), audio.with_extension("txt"));

        std::fs::write(&txt, "Ave Maria,\ngratia plena\n").unwrap();
        let from_txt = find(&audio, None);
        std::fs::write(&lrc, "In einem Bächlein helle,\n").unwrap();
        let from_lrc = find(&audio, None);
        // Synchronized lyrics come first, whichever file has them
        std::fs::write(&txt, "[00:01.00]Ave Maria!\n[00:04.00]Jungfrau mild,\n").unwrap();
        let synced_txt = find(&audio, None);
        // The extension in capitals
        std::fs::remove_file(&lrc).unwrap();
        std::fs::remove_file(&txt).unwrap();
        std::fs::write(audio.with_extension("TXT"), "Ave Maria").unwrap();
        let from_capitals = find(&audio, None);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            from_txt,
            Some(Lyrics::Plain("Ave Maria,\ngratia plena".to_string()))
        );
        assert_eq!(
            from_lrc,
            Some(Lyrics::Plain("In einem Bächlein helle,".to_string()))
        );
        assert_eq!(
            synced_txt,
            Some(Lyrics::Synced(vec![
                line(1000, None, "Ave Maria!"),
                line(4000, None, "Jungfrau mild,"),
            ]))
        );
        assert_eq!(from_capitals, Some(Lyrics::Plain("Ave Maria".to_string())));
    }

    #[test]
    fn sylt_frames_are_read_for_the_lyrics_only() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-lyrics-XXXXXX")).unwrap();
        let audio = dir.join("Die Forelle.wav");
        let sylt = sylt_frame(&[
            ("In ", 1000),
            ("einem ", 1400),
            ("Bächlein helle,", 1800),
            ("\nDa ", 4000),
            ("schoß", 4300),
        ]);
        tagged_wav(&audio, Some("Ave Maria"), Some(sylt));
        let found = find(&audio, None);
        let scanned = read_file_tags(&audio, ReadOptions::TAGS).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            found,
            Some(Lyrics::Synced(vec![
                line(1000, None, "In einem Bächlein helle,"),
                line(4000, None, "Da schoß"),
            ]))
        );
        assert_eq!(scanned.extra.synced_lyrics, None);
    }

    // Each line is a row of a list, so a hundred thousand of them took the
    // window seconds to show
    #[test]
    fn too_many_synchronized_lines_are_plain_lyrics() {
        let lrc: String = (0..MAX_SYNCED_LINES + 1)
            .map(|i| format!("[{}:{:02}.{:02}]la\n", i / 6000, i / 100 % 60, i % 100))
            .collect();
        assert!(matches!(parse(&lrc), Some(Lyrics::Plain(_))));

        // A SYLT frame with too many lines, or larger than a file of lyrics
        // that's read, gives way to the lyrics in the tags
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-lyrics-XXXXXX")).unwrap();
        let audio = dir.join("Die Forelle.wav");
        let many: Vec<(&str, u32)> = (0..=MAX_SYNCED_LINES as u32).map(|ms| ("la", ms)).collect();
        tagged_wav(&audio, Some("Ave Maria"), Some(sylt_frame(&many)));
        let found_many = find(&audio, None);
        let long = "la ".repeat(200_000);
        let large = [(long.as_str(), 1000), (long.as_str(), 2000)];
        tagged_wav(&audio, Some("Ave Maria"), Some(sylt_frame(&large)));
        let found_large = find(&audio, None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found_many, Some(Lyrics::Plain("Ave Maria".to_string())));
        assert_eq!(found_large, Some(Lyrics::Plain("Ave Maria".to_string())));
    }

    #[test]
    fn lrc_lines_are_timed() {
        let lrc = "[ti:Die Forelle]\n[offset:+500]\n\
            [00:12.00]<00:12.00>In <00:12.40>einem Bächlein helle,\n\
            [00:12.00]In a bright little brook,\n\
            [00:16.50][01:16.50]Da schoß in froher Eil\n\
            [00:20.00]\n\
            [00:25.00]Die launische Forelle\n\
            The capricious trout\n";
        assert_eq!(
            parse(lrc),
            Some(Lyrics::Synced(vec![
                line(
                    11_500,
                    None,
                    "In einem Bächlein helle,\nIn a bright little brook,"
                ),
                line(16_000, Some(19_500), "Da schoß in froher Eil"),
                line(24_500, None, "Die launische Forelle\nThe capricious trout"),
                line(76_000, None, "Da schoß in froher Eil"),
            ]))
        );

        // Too few lines with a time
        assert_eq!(
            parse("[00:00.00]Ave Maria\r\nGratia plena\r\n\r\nDominus tecum\r\n"),
            Some(Lyrics::Plain(
                "Ave Maria\nGratia plena\n\nDominus tecum".to_string()
            ))
        );
        assert_eq!(parse("[ar:Franz Schubert]\n\n"), None);
    }

    #[test]
    fn a_line_with_too_many_times_is_not_copied_for_each() {
        // A refrain sung a thousand times is still synchronized
        let times: String = (0..1000)
            .map(|second| format!("[{}:{:02}]", second / 60, second % 60))
            .collect();
        assert!(matches!(
            parse(&format!("{times}Ave Maria\n")),
            Some(Lyrics::Synced(lines)) if lines.len() == 1000
        ));

        // 100,000 copies of 500 kB would take 50 GB
        let text = "Ave Maria ".repeat(50_000);
        let lrc = format!("{}{text}", "[0:0]".repeat(100_000));
        assert_eq!(parse(&lrc), Some(Lyrics::Plain(text.trim().to_string())));
    }

    #[test]
    fn a_line_going_on_from_too_many_times_is_not_copied_for_each() {
        let times = "[0:0]".repeat(40_000);
        // Its translation would be copied to each of its 40,000 times
        let translation = "Hail Mary, full of grace, the Lord is with thee; blessed art thou \
            among women, and blessed";
        assert_eq!(
            parse(&format!("{times}Ave\n{translation}\n")),
            Some(Lyrics::Plain(format!("Ave\n{translation}")))
        );
        // And so would each of 1,000 lines
        let lines = "\ngratia plena".repeat(1000);
        assert_eq!(
            parse(&format!("{times}Ave Maria{lines}")),
            Some(Lyrics::Plain(format!("Ave Maria{lines}")))
        );
    }

    #[test]
    fn sylt_entries_are_lines_or_words() {
        let entries = |entries: &[(u64, &str)]| {
            entries
                .iter()
                .map(|(ms, text)| (Duration::from_millis(*ms), (*text).to_string()))
                .collect()
        };
        assert_eq!(
            sylt_lyrics(entries(&[(1000, "Ave Maria!"), (4000, "Jungfrau mild,")])),
            Some(Lyrics::Synced(vec![
                line(1000, None, "Ave Maria!"),
                line(4000, None, "Jungfrau mild,"),
            ]))
        );
        assert_eq!(
            sylt_lyrics(entries(&[
                (1000, "Ave "),
                (1500, "Maria!"),
                (4000, "\r\nJungfrau "),
                (4500, "mild,"),
            ])),
            Some(Lyrics::Synced(vec![
                line(1000, None, "Ave Maria!"),
                line(4000, None, "Jungfrau mild,"),
            ]))
        );
    }

    #[test]
    fn the_line_sung_is_found_by_its_time() {
        let lines = [
            line(10_000, None, "Ave Maria!"),
            line(20_000, Some(25_000), "Jungfrau mild,"),
            line(30_000, None, "Erhöre einer Jungfrau Flehen,"),
        ];
        let at = |ms| place_at(&lines, Duration::from_millis(ms));
        assert_eq!(at(0), Place::Start);
        assert_eq!(at(5_000), Place::Start);
        assert_eq!(at(10_000), Place::Line(0));
        assert_eq!(at(19_999), Place::Line(0));
        assert_eq!(at(20_000), Place::Line(1));
        assert_eq!(at(26_000), Place::Pause);
        assert_eq!(at(31_000), Place::Line(2));

        // After the last line, once it has ended, and in lyrics without
        // lines
        let ended = [line(10_000, Some(15_000), "Ave Maria!")];
        assert_eq!(
            place_at(&ended, Duration::from_millis(16_000)),
            Place::Pause
        );
        assert_eq!(place_at(&[], Duration::from_millis(5_000)), Place::Start);

        let next = |ms| next_change(&lines, Duration::from_millis(ms)).map(|d| d.as_millis());
        assert_eq!(next(5_000), Some(10_000));
        assert_eq!(next(10_000), Some(20_000));
        assert_eq!(next(21_000), Some(25_000));
        assert_eq!(next(26_000), Some(30_000));
        assert_eq!(next(31_000), None);
    }

    #[test]
    fn a_file_too_large_for_lyrics_is_left_out() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-lyrics-XXXXXX")).unwrap();
        let audio = dir.join("Ave Maria.flac");
        std::fs::write(audio.with_extension("lrc"), "Ave ".repeat(300_000)).unwrap();
        let found = find(&audio, None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, None);
    }

    #[test]
    fn lyrics_too_large_in_the_tags_are_left_out() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-lyrics-XXXXXX")).unwrap();
        let audio = dir.join("Ave Maria.wav");
        tagged_wav(&audio, Some(&"Ave ".repeat(300_000)), None);
        let found = find(&audio, None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, None);
    }

    #[test]
    fn times_are_taken_out_of_lines() {
        let lines = [
            (
                "[00:12.00]In einem Bächlein helle",
                "In einem Bächlein helle",
            ),
            ("[0:12]In einem Bächlein helle", "In einem Bächlein helle"),
            ("[00:12.5]Da schoß in froher Eil", "Da schoß in froher Eil"),
            (
                "[00:12.500]Da schoß in froher Eil",
                "Da schoß in froher Eil",
            ),
            ("[00:12:50]Da schoß in froher Eil", "Da schoß in froher Eil"),
            // Past the first hour
            ("[125:07.25]Die launische Forelle", "Die launische Forelle"),
            // The longest times
            (
                "[99999:59.999]Die launische Forelle",
                "Die launische Forelle",
            ),
            (
                "[00:12.00]<99999:59.999>In einem Bächlein",
                "In einem Bächlein",
            ),
            (
                "[00:20.00][01:05.50]Vorüber wie ein Pfeil",
                "Vorüber wie ein Pfeil",
            ),
            (
                "  [00:20.00] Vorüber wie ein Pfeil",
                "Vorüber wie ein Pfeil",
            ),
            (
                "[00:12.00]<00:12.00>In <00:12.40>einem <00:12.80>Bächlein",
                "In einem Bächlein",
            ),
            (
                "[00:12.00]<00:12.00> In <00:12.40> einem <00:12.80> Bächlein <00:13.20>",
                "In einem Bächlein",
            ),
            ("[00:30.00]", ""),
        ];
        for (line, text) in lines {
            assert_eq!(text_without_times(line), text, "{line}");
        }
    }

    #[test]
    fn other_brackets_stay() {
        for line in [
            "[Chorus: Tutti]",
            "[Refrain]",
            "Ave Maria, gratia plena",
            // Not times: seconds past 59, a fraction too long, no minutes
            "[00:61.00]Ave Maria",
            "[00:12.5000]Ave Maria",
            "[:12]Ave Maria",
            "<00:12.0x>Ave Maria",
            "Dominus tecum < benedicta tu",
            // Too long for a time, with the bracket further on than one
            "[12345678901234567890]Ave Maria",
            "<12345678901234567890>Ave Maria",
            "<00:12.00 benedicta tu>Ave Maria",
            // Not a header tag
            "[Solo: Sopran]",
        ] {
            assert!(
                matches!(text_without_times(line), Cow::Borrowed(text) if text == line),
                "{line}"
            );
        }
    }

    #[test]
    fn a_run_of_brackets_is_no_time() {
        // Looking for the closing bracket of each one would go through the
        // rest of the line 100,000 times
        let run = "<".repeat(100_000);
        for line in [run.clone(), format!("{run}>"), format!("[00:12.00]{run}")] {
            let text = line.strip_prefix("[00:12.00]").unwrap_or(&line);
            assert!(matches!(without_word_times(text), Cow::Borrowed(t) if t == text));
            assert_eq!(text_without_times(&line), text);
        }
    }

    #[test]
    fn headers_go_with_their_lines() {
        let lrc = "[ar:Franz Schubert]\r\n[ti:Die Forelle]\r\n[lr:Christian Friedrich Daniel Schubart]\r\n[offset:+250]\r\n[length: 02:10]\r\n[00:12.00]In einem Bächlein helle,\r\n\r\n[00:16.50]Da schoß in froher Eil\r\n";
        assert_eq!(
            text_without_times(lrc),
            "In einem Bächlein helle,\r\n\r\nDa schoß in froher Eil\r\n"
        );
    }
}
