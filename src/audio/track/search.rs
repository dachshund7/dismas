// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The text that searching the queue and the album grid looks into

use super::*;
use crate::lyrics;

// Makes searching insensitive to case, accents, the kinds of quotes and
// dashes, and spacing: "Gemüte" becomes "gemute", "Bach’s" "bach's" and
// "Kyrie – Christe" "kyrie - christe", so that they can be typed without
// the accent, on any keyboard. Runs of whitespace become one space, which
// the search syntax tells phrases and the values of fields apart by. Most
// tags are ASCII, which only needs lowercasing
pub fn normalize_search_text(s: &str) -> String {
    let mut folded = String::with_capacity(s.len());
    if s.is_ascii() {
        // Whitespace as `char::is_whitespace()` has it, which counts the
        // vertical tab, unlike `u8::is_ascii_whitespace()`
        let mut space = false;
        for byte in s.bytes() {
            if matches!(byte, b' ' | b'\t'..=b'\r') {
                space = !folded.is_empty();
            } else {
                if space {
                    folded.push(' ');
                    space = false;
                }
                folded.push(char::from(byte.to_ascii_lowercase()));
            }
        }
    } else {
        fold_into(s, &mut folded, true);
    }
    folded
}

// The fold of the keys that tell composers, performers and works apart,
// which plays.db keeps the favorite works by: case and accents only, as
// before the search folded quotes and dashes, so that the keys don't change
pub fn normalize_key_text(s: &str) -> String {
    let mut folded = String::with_capacity(s.len());
    fold_into(s, &mut folded, false);
    folded
}

// The text without case and accents, and for a `search`, with the kinds of
// quotes and dashes made one, and runs of whitespace one space, without
// any at the ends
fn fold_into(s: &str, folded: &mut String, search: bool) {
    // The compatibility decomposition splits accented letters into the
    // letter and a combining mark, which we drop
    let decomposed = glib::normalize(s, glib::NormalizeMode::All);

    let mut space = false;
    for c in decomposed.chars().flat_map(char::to_lowercase) {
        match c {
            '\u{0300}'..='\u{036f}'
            | '\u{1ab0}'..='\u{1aff}'
            | '\u{1dc0}'..='\u{1dff}'
            | '\u{20d0}'..='\u{20ff}'
            | '\u{fe20}'..='\u{fe2f}' => continue,
            _ if search && c.is_whitespace() => {
                space = !folded.is_empty();
                continue;
            }
            _ => (),
        }
        if space {
            folded.push(' ');
            space = false;
        }
        match c {
            // Letters that don't decompose into a base letter
            'ß' => folded.push_str("ss"),
            'æ' => folded.push_str("ae"),
            'œ' => folded.push_str("oe"),
            'ø' => folded.push('o'),
            'ł' => folded.push('l'),
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{02bc}' if search => {
                folded.push('\'');
            }
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' if search => folded.push('"'),
            '\u{2010}'..='\u{2015}' | '\u{2212}' if search => folded.push('-'),
            _ => folded.push(c),
        }
    }
}

// The text that searching the queue and the album grid looks into for a
// track, made the same way in both, so that they find the same tracks:
// its artists, its title, or else the title its file name gives it, the
// text of its tags (`track_search_text()`), and the folder its album is
// named after when it has no album tag. What the queue shows in place of
// what's missing, like "Unknown artist" or "Track 3", isn't searched, as
// the library can't keep it in the language of the moment
pub fn search_key(
    artists: Option<&str>,
    title: Option<&str>,
    tag_text: &str,
    folder_album: Option<&str>,
) -> String {
    let text = [
        artists.unwrap_or_default(),
        title.unwrap_or_default(),
        tag_text,
        folder_album.unwrap_or_default(),
    ]
    .join(" ");
    normalize_search_text(&text)
}

// The text of the tags of a track that searching looks into: the text of
// its tags, and the names of the works the "Work Hierarchy" plugin gives,
// which lofty leaves out of `tag`
pub fn track_search_text(tag: &Tag, credits: &TrackCredits) -> String {
    let mut text = tag_search_text(tag);
    let works = credits.work_names.join(" ");
    if !works.is_empty() {
        text.push(' ');
        text.push_str(text_start(&works, MAX_SEARCHED_TEXT));
    }
    // The catalogue numbers as one word too, e.g. "bwv1047", as they're
    // often typed without the space
    let title = track_title(tag);
    for word in fields::catalogue_numbers(credits, title.as_deref())
        .iter()
        .filter_map(crate::catalogue::CatalogueNumber::word)
    {
        text.push(' ');
        text.push_str(&word);
    }
    text
}

// The text of all the tags of a track, for searching the queue; this
// leaves out the tags that only contain identifiers, numbers or technical
// data, as they would make short searches, like "op 29", match by accident
fn tag_search_text(tag: &Tag) -> String {
    let texts: Vec<Cow<str>> = tag
        .items()
        .filter(|item| {
            !matches!(
                item.key(),
                // The album title and artist are handled below
                ItemKey::AlbumTitle
                    | ItemKey::AlbumArtist
                    | ItemKey::AlbumArtists
                    | ItemKey::TrackNumber
                    | ItemKey::TrackTotal
                    | ItemKey::DiscNumber
                    | ItemKey::DiscTotal
                    | ItemKey::MovementNumber
                    | ItemKey::MovementTotal
                    | ItemKey::Isrc
                    | ItemKey::Barcode
                    | ItemKey::AcoustId
                    | ItemKey::AcoustIdFingerprint
                    | ItemKey::MusicBrainzRecordingId
                    | ItemKey::MusicBrainzTrackId
                    | ItemKey::MusicBrainzReleaseId
                    | ItemKey::MusicBrainzReleaseGroupId
                    | ItemKey::MusicBrainzArtistId
                    | ItemKey::MusicBrainzReleaseArtistId
                    | ItemKey::MusicBrainzWorkId
                    | ItemKey::MusicBrainzReleaseType
                    | ItemKey::ReplayGainAlbumGain
                    | ItemKey::ReplayGainAlbumPeak
                    | ItemKey::ReplayGainTrackGain
                    | ItemKey::ReplayGainTrackPeak
                    | ItemKey::EncoderSoftware
                    | ItemKey::EncoderSettings
                    | ItemKey::EncodingTime
                    | ItemKey::TaggingTime
                    | ItemKey::Length
                    | ItemKey::Bpm
                    | ItemKey::IntegerBpm
                    | ItemKey::Popularimeter
                    | ItemKey::FlagCompilation
                    | ItemKey::FlagPodcast
                    | ItemKey::AlbumTitleSortOrder
                    | ItemKey::AlbumArtistSortOrder
                    | ItemKey::TrackTitleSortOrder
                    | ItemKey::TrackArtistSortOrder
                    | ItemKey::ShowNameSortOrder
                    | ItemKey::ComposerSortOrder
            )
        })
        .filter_map(|item| {
            let text = text_start(item.value().text()?, MAX_SEARCHED_TEXT);
            // The times of synchronized lyrics would make searching for
            // a number match them. They're taken out of the start that's
            // kept, which is never longer than it
            Some(match item.key() {
                ItemKey::Lyrics | ItemKey::UnsyncLyrics => lyrics::text_without_times(text),
                _ => Cow::Borrowed(text),
            })
        })
        .collect();
    let mut text = texts
        .iter()
        // Links, like the one to the video of a download from YouTube,
        // would make searching for their site match every track from it
        .flat_map(|text| text.split_whitespace())
        .filter(|word| !is_link(word))
        .join(" ");

    let album_artist = tag.get_string(ItemKey::AlbumArtist);
    if let Some(album_artist) = album_artist {
        text.push(' ');
        text.push_str(&album_artist_search_text(tag, album_artist));
    }

    if let Some(album) = tag.get_string(ItemKey::AlbumTitle) {
        text.push(' ');
        text.push_str(&album_title_search_text(album, album_artist));
    }

    text
}

// How much of each tag is searched: enough for the lyrics of a song, while
// a crafted comment of megabytes, which the tracks of a CUE sheet would
// each copy, isn't kept for every track
const MAX_SEARCHED_TEXT: usize = 16 * 1024;

// The text up to `max` bytes, cut between characters
fn text_start(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

fn is_link(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    ["http://", "https://", "www."]
        .iter()
        .any(|prefix| word.starts_with(prefix))
}

// Album titles often list the names in the album artist, e.g. the album
// "Lotti, Zelenka, Bach" by "Antonio Lotti, Jan Dismas Zelenka, Johann
// Sebastian Bach; …"; searching for one of the composers would then match
// every track of the album. Those names are already searched in the tags of
// each track, so we leave them out of the album title
fn album_title_search_text(album: &str, album_artist: Option<&str>) -> String {
    let Some(album_artist) = album_artist else {
        return album.to_string();
    };

    let names = normalize_search_text(album_artist);
    let names: std::collections::HashSet<&str> = search_words(&names).collect();

    search_words(album)
        .filter(|word| !names.contains(normalize_search_text(word).as_str()))
        .join(" ")
}

// The words of a text, without the punctuation around them
fn search_words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

// The album artist of classical releases tagged by MusicBrainz Picard lists
// the composers and then everyone who performs on the release, e.g.
// "Dieterich Buxtehude; Ton Koopman, Amsterdam Baroque Choir"; so it would
// make a search for a performer match every track of the release, even the
// ones they don't play on. We only use it to fill in for the tags a track
// doesn't have: the composers if it has no composer, and the performers if
// it has no performer or conductor, as they might only be credited there
fn album_artist_search_text(tag: &Tag, album_artist: &str) -> String {
    let (composers, performers) = match album_artist.split_once(';') {
        Some((composers, performers)) => (composers, performers),
        // Without a separator, it's the artist of the whole release
        None => ("", album_artist),
    };

    let has_composer = tag.get_string(ItemKey::Composer).is_some();
    let has_performer = tag.get_string(ItemKey::Performer).is_some()
        || tag.get_string(ItemKey::Conductor).is_some();

    let mut text = String::new();
    if !has_composer {
        text.push_str(composers);
    }
    if !has_performer {
        text.push(' ');
        text.push_str(performers);
    }

    text
}

#[cfg(test)]
mod tests {
    use lofty::tag::TagType;
    use rand::{RngExt, SeedableRng, rngs::StdRng};

    use super::*;

    // The folds as they were before they were made in one pass, which the
    // library and plays.db keep values made with, so the new ones must
    // give exactly the same
    fn old_normalize_search_text(s: &str) -> String {
        let folded = if s.is_ascii() {
            s.to_ascii_lowercase()
        } else {
            old_fold_letters(s)
                .chars()
                .map(|c| match c {
                    '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{02bc}' => '\'',
                    '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
                    '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
                    _ => c,
                })
                .collect()
        };
        folded.split_whitespace().join(" ")
    }

    fn old_fold_letters(s: &str) -> String {
        let decomposed = glib::normalize(s, glib::NormalizeMode::All);
        let mut res = String::with_capacity(decomposed.len());
        for c in decomposed.chars().flat_map(char::to_lowercase) {
            match c {
                '\u{0300}'..='\u{036f}'
                | '\u{1ab0}'..='\u{1aff}'
                | '\u{1dc0}'..='\u{1dff}'
                | '\u{20d0}'..='\u{20ff}'
                | '\u{fe20}'..='\u{fe2f}' => (),
                'ß' => res.push_str("ss"),
                'æ' => res.push_str("ae"),
                'œ' => res.push_str("oe"),
                'ø' => res.push('o'),
                'ł' => res.push('l'),
                _ => res.push(c),
            }
        }
        res
    }

    // Characters that the folds treat in their own ways
    const TRICKY: &[char] = &[
        ' ', '\t', '\n', '\u{0b}', '\u{0c}', '\r', '\u{1f}', '\u{85}', '\u{a0}', '\u{1680}',
        '\u{2002}', '\u{2009}', '\u{200b}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{3000}',
        '\u{0301}', '\u{0308}', '\u{1ab0}', '\u{20dd}', '\u{fe20}', 'ß', 'ẞ', 'Æ', 'æ', 'Œ', 'ø',
        'Ø', 'Ł', 'ł', 'İ', 'ı', 'Σ', 'ς', 'ﬁ', 'é', 'Ü', '½', '²', '\u{2018}', '\u{2019}',
        '\u{201a}', '\u{201b}', '\u{02bc}', '\u{201c}', '\u{201d}', '\u{201e}', '\u{201f}',
        '\u{2010}', '\u{2011}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}', '\'', '"', '-',
        'A', 'z', '7', 'Ж', '日',
    ];

    #[test]
    fn the_folds_in_one_pass_are_the_same_as_before() {
        let check = |s: &str| {
            assert_eq!(
                normalize_search_text(s),
                old_normalize_search_text(s),
                "{s:?}"
            );
            assert_eq!(normalize_key_text(s), old_fold_letters(s), "{s:?}");
        };
        for s in [
            "",
            " ",
            " \t\u{0b}\n ",
            "\u{a0}\u{3000}",
            "Bach\u{0b}Partita",
            "  Kyrie \u{2013}  Christe ",
            "e\u{0301} \u{0301} x",
            "\u{0301}",
            " \u{0301} ",
            "Straße",
            "İstanbul",
            "ΣΟΦΟΣ",
            "ﬁne",
            "Bach’s „Wohltemperiertes“ Klavier",
            "Johann Sebastian Bach (1685\u{2212}1750)",
        ] {
            check(s);
        }
        let mut rng = StdRng::seed_from_u64(1);
        for _ in 0..100_000 {
            let len = rng.random_range(0..12);
            let s: String = (0..len)
                .map(|_| {
                    if rng.random_range(0..3) == 0 {
                        char::from(rng.random_range(0x20u8..0x7f))
                    } else {
                        TRICKY[rng.random_range(0..TRICKY.len())]
                    }
                })
                .collect();
            check(&s);
        }
    }

    // Times the folds before and after they were made in one pass:
    //   cargo test --release time_normalize_search_text -- --ignored --nocapture
    #[test]
    #[ignore]
    fn time_normalize_search_text() {
        let texts: Vec<String> = [
            "Johann Sebastian Bach",
            "Partita No. 2 in D minor, BWV 1004: V. Chaconne",
            "Berliner Philharmoniker",
            "Wohltemperiertes Klavier I – Präludium und Fuge Nr. 1 C-Dur",
            "Antonín Dvořák",
            "Ensemble Café Zimmermann",
            "Bach’s „Kunst der Fuge“",
            "Sergei Rachmaninoff · Piano Concerto No. 2 in C minor, Op. 18",
        ]
        .iter()
        .flat_map(|text| (0..2_000).map(move |i| format!("{text} {i}")))
        .collect();
        let time = |fold: &dyn Fn(&str) -> String| {
            let start = std::time::Instant::now();
            let total: usize = texts.iter().map(|text| fold(text).len()).sum();
            assert!(total > 0);
            start.elapsed()
        };
        // The faster of two runs each, so that warming up doesn't count
        let before = time(&old_normalize_search_text).min(time(&old_normalize_search_text));
        let after = time(&normalize_search_text).min(time(&normalize_search_text));
        println!(
            "normalize_search_text: {:.1} ms before, {:.1} ms after",
            before.as_secs_f64() * 1000.0,
            after.as_secs_f64() * 1000.0
        );
    }

    #[test]
    fn search_text_has_no_links() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::TrackTitle, "Aqua".to_string());
        tag.insert_text(
            ItemKey::Comment,
            "https://www.youtube.com/watch?v=LLIsy64Sv5c".to_string(),
        );
        let text = normalize_search_text(&tag_search_text(&tag));
        assert!(!text.contains("youtube"));
        assert!(text.contains("aqua"));
    }

    #[test]
    fn search_text_keeps_the_start_of_long_tags() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::TrackTitle, "Aqua".to_string());
        // Two-byte characters, so that the limit falls inside one
        tag.insert_text(ItemKey::Comment, "é".repeat(20_000));
        let text = tag_search_text(&tag);
        assert!(text.contains("Aqua"));
        assert!(text.len() < 16 * 1024 + 100);
        assert_eq!(text_start("éa", 1), "");
        assert_eq!(text_start("éa", 3), "éa");
    }

    #[test]
    fn search_text_has_no_times_of_lyrics() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(
            ItemKey::Lyrics,
            "[ti:Die Forelle]\n[00:12.00]In einem Bächlein helle".to_string(),
        );
        tag.insert_text(
            ItemKey::UnsyncLyrics,
            "[00:16.50]Da schoß in <00:17.00>froher Eil".to_string(),
        );
        let text = tag_search_text(&tag);
        assert_eq!(text, "In einem Bächlein helle Da schoß in froher Eil");
    }

    #[test]
    fn album_title_leaves_out_album_artist_names() {
        let artist = "Antonio Lotti, Jan Dismas Zelenka, Johann Sebastian Bach; Thomas Hengelbrock, Balthasar‐Neumann‐Chor";
        assert_eq!(
            album_title_search_text("Lotti, Zelenka, Bach", Some(artist)),
            ""
        );
        assert_eq!(
            album_title_search_text(
                "Images: Hommage à Marcel Tournier",
                Some("Marcel Tournier; Emmanuel Ceysson")
            ),
            "Images Hommage à"
        );
        assert_eq!(
            album_title_search_text(
                "Cantatas, Volume 12",
                Some("Johann Sebastian Bach; Ton Koopman")
            ),
            "Cantatas Volume 12"
        );
        assert_eq!(
            album_title_search_text("Il giardino di rose", None),
            "Il giardino di rose"
        );
    }

    #[test]
    fn search_text_ignores_case_and_accents() {
        assert_eq!(
            normalize_search_text("Tröste mir, Jesu, mein Gemüte"),
            "troste mir, jesu, mein gemute"
        );
        assert_eq!(normalize_search_text("Antonín Dvořák"), "antonin dvorak");
    }

    #[test]
    fn search_text_folds_quotes_dashes_and_spaces() {
        assert_eq!(normalize_search_text("L’Étang du Parc"), "l'etang du parc");
        assert_eq!(
            normalize_search_text("Kantate „Ach Gott“"),
            "kantate \"ach gott\""
        );
        assert_eq!(
            normalize_search_text("Kyrie – Christe — Kyrie"),
            "kyrie - christe - kyrie"
        );
        assert_eq!(normalize_search_text("−5 dB"), "-5 db");
        assert_eq!(
            normalize_search_text("  Ach\tGott,\n  vom   Himmel "),
            "ach gott, vom himmel"
        );
    }

    #[test]
    fn ascii_text_folds_like_any_text() {
        // The fast path for ASCII gives what the general one would
        for text in [
            "Bach: BWV 1047",
            "  AC/DC\tLive ",
            "Op. 27, No. 2",
            "~!@#$%^&*()_+",
        ] {
            assert_eq!(
                normalize_search_text(text),
                normalize_key_text(text).split_whitespace().join(" "),
                "{text}"
            );
        }
    }

    #[test]
    fn composer_keys_keep_typographic_punctuation() {
        // plays.db keeps favorite works by keys made from composer keys, so
        // they mustn't change with the search's folds
        assert_eq!(normalize_key_text("Conlon O’Brien"), "conlon o’brien");
        assert_eq!(normalize_key_text("Saint‐Saëns"), "saint‐saens");
    }

    #[test]
    fn search_text_expands_letters_without_decomposition() {
        assert_eq!(normalize_search_text("Groß"), "gross");
        assert_eq!(
            normalize_search_text("Œuvres complètes"),
            "oeuvres completes"
        );
        assert_eq!(normalize_search_text("Søren Łukasz"), "soren lukasz");
    }
}
