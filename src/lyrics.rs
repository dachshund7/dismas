// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Lyrics, as the tags and LRC files give them. Synchronized lyrics, in
//! the LRC format, have the time each line is sung at before it, e.g.
//! "[01:23.45]", can have the time of each word in the line, "<01:23.45>",
//! and start with header lines, like "[ar:Franz Schubert]"

use std::{borrow::Cow, time::Duration};

// The header lines of LRC files: the artist, the album, the title, the
// author, the lyricist, the maker of the file, the length of the track,
// the offset of the times, the program that made the file and its
// version, and comments
const HEADER_TAGS: [&str; 12] = [
    "ar", "al", "ti", "au", "lr", "by", "length", "offset", "re", "tool", "ve", "#",
];

// The text of lyrics without the times of synchronized lyrics and without
// their header lines, e.g. for searching, where "[01:12.50]" would match
// a search for "12". Other text in brackets, like "[Chorus]", stays
pub fn text_without_times(text: &str) -> Cow<'_, str> {
    if !text.contains(['[', '<']) {
        return Cow::Borrowed(text);
    }
    let mut lines = String::with_capacity(text.len());
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if is_header(content) {
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

// Whether a line is a header of an LRC file, like "[ar:Franz Schubert]"
fn is_header(line: &str) -> bool {
    let Some(inner) = line
        .trim()
        .strip_prefix('[')
        .and_then(|line| line.strip_suffix(']'))
    else {
        return false;
    };
    inner
        .split_once(':')
        .is_some_and(|(tag, _)| HEADER_TAGS.contains(&tag.trim().to_ascii_lowercase().as_str()))
}

fn line_without_times(line: &str) -> Cow<'_, str> {
    let text = without_leading_times(line);
    if text.contains('<') {
        without_word_times(text)
    } else {
        Cow::Borrowed(text)
    }
}

// The text of a line after the times it's sung at, of which there can be
// several when the line comes back, e.g. "[00:12.00][01:05.50]Refrain"
fn without_leading_times(line: &str) -> &str {
    let mut text = line.trim_start();
    let mut stripped = false;
    while let Some((time, after)) = bracketed(text, '[', ']')
        && timestamp(time).is_some()
    {
        text = after;
        stripped = true;
    }
    // A space after the times only sets the text apart from them
    if stripped { text.trim_start() } else { line }
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

// The text between `open`, at the start of `text`, and the next `close`,
// and what follows
fn bracketed(text: &str, open: char, close: char) -> Option<(&str, &str)> {
    let inner = text.strip_prefix(open)?;
    let end = inner.find(close)?;
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
    use super::*;

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
    fn headers_go_with_their_lines() {
        let lrc = "[ar:Franz Schubert]\r\n[ti:Die Forelle]\r\n[lr:Christian Friedrich Daniel Schubart]\r\n[offset:+250]\r\n[length: 02:10]\r\n[00:12.00]In einem Bächlein helle,\r\n\r\n[00:16.50]Da schoß in froher Eil\r\n";
        assert_eq!(
            text_without_times(lrc),
            "In einem Bächlein helle,\r\n\r\nDa schoß in froher Eil\r\n"
        );
    }
}
