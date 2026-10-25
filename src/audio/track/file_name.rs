// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The title and the album of a track without them in its tags, from
//! the names of its file and its folder

use super::*;

// The album of a track without one in its tags: the folder it's in, as in
// the album grid
pub(super) fn fallback_album(path: &Path) -> String {
    folder_album_title(path).unwrap_or_else(|| i18n("Unknown album"))
}

// The title of a track without one in its tags, from the name of its file,
// e.g. "Intro" for "01 - Intro.flac". Only a leading track number is left
// out: "Song - Live" could just as well be "Artist - Song", so the rest is
// kept as it is. This can read the folder of the file, so it must not be
// called on the main thread
pub fn title_from_file_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    title_from_name(&stem, |width| has_padded_numbers(path, width))
}

// `padded(width)` tells whether the other names in the folder start with
// numbers padded with zeros to `width` digits
pub(super) fn title_from_name(stem: &str, padded: impl FnOnce(usize) -> bool) -> String {
    let name = stem.replace('_', " ");
    let name = name.trim();
    without_track_number(name, padded)
        .unwrap_or(name)
        .to_string()
}

// The rest of a name that starts with a track number, e.g. "01 - ", "1. ",
// "3) ", "01 ", or "1-03 " for a disc and a track; the numbers that can be
// part of a title, like in "99 Luftballons" or "3.14 Pi", are kept
fn without_track_number(name: &str, padded: impl FnOnce(usize) -> bool) -> Option<&str> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();

    let n = digits(name);
    if !(1..=3).contains(&n) {
        return None;
    }
    let (number, rest) = name.split_at(n);
    // A disc and a track, e.g. "1-03"
    if let Some(track) = rest
        .strip_prefix('-')
        .filter(|track| n <= 2 && digits(track) == 2)
    {
        return after_track_number(&track[2..], || true);
    }
    // A number followed by a space alone is a track number if it starts
    // with a zero, e.g. "01 Intro", or if the other names in the folder
    // do, e.g. "10 Song" next to "09 Song"
    after_track_number(rest, || (n >= 2 && number.starts_with('0')) || padded(n))
}

// The rest of a name after the separator that follows its track number:
// a dot, a dash or a parenthesis, or if `bare_space()`, a space
fn after_track_number(rest: &str, bare_space: impl FnOnce() -> bool) -> Option<&str> {
    let trimmed = rest.trim_start();
    let after = match trimmed.strip_prefix(['.', '-', ')']) {
        Some(after) => {
            let spaced = after.trim_start();
            // A digit right after the separator continues the number, e.g.
            // "3.14 Pi" or "2-4-6-8 Motorway"; after a space, it starts the
            // title, e.g. "02 - 12 to 12"
            if spaced.len() == after.len() && after.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            spaced
        }
        None if trimmed.len() < rest.len() && bare_space() => trimmed,
        None => return None,
    };
    (!after.is_empty()).then_some(after)
}

// Whether another name in the folder of `path` starts with a number padded
// with zeros to `width` digits, e.g. "09 Song"
fn has_padded_numbers(path: &Path, width: usize) -> bool {
    let Some(entries) = path.parent().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        name.len() > width
            && name[0] == b'0'
            && name[..width].iter().all(u8::is_ascii_digit)
            && !name[width].is_ascii_digit()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_lose_only_their_track_numbers() {
        let cases = [
            ("01 - Intro", "Intro"),
            ("01. Intro", "Intro"),
            ("01 Intro", "Intro"),
            ("01-Intro", "Intro"),
            ("03_Song_Name", "Song Name"),
            ("1-03 Intro", "Intro"),
            ("3) Intro", "Intro"),
            ("10 - Intro", "Intro"),
            ("Song - Live", "Song - Live"),
            ("01 - Song - Live", "Song - Live"),
            ("Artist - Song", "Artist - Song"),
            ("Song.Name", "Song.Name"),
            // Titles starting with a number
            ("02 12 to 12", "12 to 12"),
            ("02 - 12 to 12", "12 to 12"),
            // Numbers that can be part of the title
            ("99 Luftballons", "99 Luftballons"),
            ("2001 A Space Odyssey", "2001 A Space Odyssey"),
            ("2-4-6-8 Motorway", "2-4-6-8 Motorway"),
            ("3.14 Pi", "3.14 Pi"),
            // Nothing would be left
            ("01", "01"),
            ("01 - ", "01 -"),
        ];
        for (name, title) in cases {
            assert_eq!(title_from_name(name, |_| false), title, "{name}");
        }

        // Next to names that start with "01" to "09"
        assert_eq!(
            title_from_name("10 under the mat", |width| width == 2),
            "under the mat"
        );
        assert_eq!(
            title_from_name("99 Luftballons", |width| width == 3),
            "99 Luftballons"
        );
    }
}
