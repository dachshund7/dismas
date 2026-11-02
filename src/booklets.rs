// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The booklets of albums: the PDF files next to their tracks. Stores name
// them in their own ways, "booklet.pdf", "Digital Booklet - <album>.pdf"
// or a number, so any PDF counts, whatever its name

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use gtk::{gio, glib, prelude::*};

use crate::{cue_sheet, i18n::i18n, library::disc_album_folder, utils};

// The action that opens a booklet, with the URI of its file
pub(crate) const OPEN_ACTION: &str = "win.open-booklet";

// A folder with more PDF files than this isn't an album's, but one like
// Downloads that a track was opened from: its PDFs aren't booklets, and a
// menu of them all would be as wide as the longest name
const MAX_BOOKLETS: usize = 10;

// The PDF files in the folders of the tracks at `paths`, and in the folder
// of their album when they're in a disc folder, as in Album/CD1, where the
// album's booklet is often left; in the order a file manager shows their
// names. This reads the folders, so it must not run on the main thread
pub(crate) fn find(paths: &[PathBuf]) -> Vec<PathBuf> {
    let folders: HashSet<&Path> = paths
        .iter()
        // The folder of a CUE sheet, for its tracks
        .filter_map(|path| cue_sheet::real_path(path).parent())
        .flat_map(|dir| std::iter::once(dir).chain(disc_album_folder(dir)))
        .collect();

    let mut found: Vec<_> = folders
        .into_iter()
        .flat_map(pdf_files)
        .map(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            (utils::file_name_key(&name), path)
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, path)| path).collect()
}

// The PDF files in a folder, without the hidden ones
fn pdf_files(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = Path::new(&name);
            !name.as_os_str().as_encoded_bytes().starts_with(b".")
                && name
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        })
        .map(|entry| entry.path())
        // Following symbolic links
        .filter(|path| path.is_file())
        // Only enough to tell, as such a folder can have thousands
        .take(MAX_BOOKLETS + 1)
        .collect();
    if found.len() > MAX_BOOKLETS {
        return Vec::new();
    }
    found
}

// The item of a menu that opens the booklets: "Open Booklet" for one, and
// for several, a submenu of their names
pub(crate) fn menu_item(booklets: &[PathBuf]) -> Option<gio::MenuItem> {
    match booklets {
        [] => None,
        [booklet] => {
            let item = gio::MenuItem::new(Some(&i18n("Open Booklet")), None);
            item.set_action_and_target_value(Some(OPEN_ACTION), Some(&target(booklet)));
            Some(item)
        }
        _ => Some(gio::MenuItem::new_submenu(
            Some(&i18n("Open Booklet")),
            &menu(booklets),
        )),
    }
}

// A menu of the booklets, by their names (see `labels()`)
pub(crate) fn menu(booklets: &[PathBuf]) -> gio::Menu {
    let menu = gio::Menu::new();
    let names: Vec<String> = booklets
        .iter()
        .map(|booklet| {
            let name = booklet.file_name().unwrap_or_default();
            name.to_string_lossy().into_owned()
        })
        .collect();
    for (booklet, label) in booklets.iter().zip(labels(&names)) {
        // An underscore would underline the letter after it
        let item = gio::MenuItem::new(Some(&label.replace('_', "__")), None);
        item.set_action_and_target_value(Some(OPEN_ACTION), Some(&target(booklet)));
        menu.append_item(&item);
    }
    menu
}

// The longest name of a booklet in a menu, in characters: a popover menu
// is as wide as its widest item, even in a submenu, and stores often
// name booklets after the album
const MAX_LABEL: usize = 40;
// How much of a long name is kept at its end, where the booklets of an
// album often differ ("… CD 2")
const LABEL_END: usize = 15;
// How much comes before where names differ, when that's in the middle
const LABEL_BEFORE: usize = 14;
// How far a cut moves to fall between two words
const WORD_SLACK: usize = 8;

// The names of booklets in a menu, from their file names: without the
// extension, as they're all PDF files, and shortened when they're long,
// to their start and their end. Names that are then the same differ in
// the middle, which shows instead, and those that are still the same are
// left whole, so that the booklets can always be told apart
fn labels(names: &[String]) -> Vec<String> {
    let stems: Vec<Vec<char>> = names
        .iter()
        .map(|name| {
            let stem = Path::new(name)
                .file_stem()
                .map_or_else(|| name.clone(), |stem| stem.to_string_lossy().into_owned());
            // Composed, so that an accent written as a mark of its own,
            // as in names from macOS, isn't cut from its letter
            glib::normalize(stem, glib::NormalizeMode::DefaultCompose)
                .chars()
                .collect()
        })
        .collect();
    let mut labels: Vec<String> = stems.iter().map(|stem| shortened(stem)).collect();
    let middles: Vec<Option<String>> = (0..labels.len())
        .map(|i| {
            // Past the start it has in common with each of the others
            let differs_at = shared(&labels, i)
                .map(|j| {
                    stems[i]
                        .iter()
                        .zip(&stems[j])
                        .take_while(|(a, b)| a == b)
                        .count()
                })
                .max()?;
            Some(middle(&stems[i], differs_at))
        })
        .collect();
    for (label, middle) in labels.iter_mut().zip(middles) {
        if let Some(middle) = middle {
            *label = middle;
        }
    }
    let whole: Vec<bool> = (0..labels.len())
        .map(|i| shared(&labels, i).next().is_some())
        .collect();
    for ((label, whole), name) in labels.iter_mut().zip(whole).zip(names) {
        if whole {
            label.clone_from(name);
        }
    }
    labels
}

// The others with the same label as the one at `i`
fn shared(labels: &[String], i: usize) -> impl Iterator<Item = usize> + '_ {
    (0..labels.len()).filter(move |&j| j != i && labels[j] == labels[i])
}

// A name of at most `MAX_LABEL` characters: its start, an ellipsis and
// its end
fn shortened(name: &[char]) -> String {
    if name.len() <= MAX_LABEL {
        return name.iter().collect();
    }
    let head = after_word(name, MAX_LABEL - LABEL_END - 1, false);
    let tail = after_word(name, name.len() - LABEL_END, true);
    joined(&name[..head], &name[tail..], false, true)
}

// The part of a name of at most `MAX_LABEL` characters that shows where it
// differs from others, at `at`, with ellipses where it's cut
fn middle(name: &[char], at: usize) -> String {
    let start = at.saturating_sub(LABEL_BEFORE);
    let start = if start == 0 {
        0
    } else {
        after_word(name, start, true)
    };
    let mut end = (start + MAX_LABEL - 2).min(name.len());
    if end < name.len() {
        end = after_word(name, end, false).max(at + 1).min(name.len());
    }
    joined(&name[start..end], &[], start > 0, end < name.len())
}

// Moves a cut at `at` to fall between two words, when there's a space
// within `WORD_SLACK` characters: forward, past the space, for the start
// of a part, or back to it for the end of one
fn after_word(name: &[char], at: usize, forward: bool) -> usize {
    if forward {
        let near = &name[at..(at + WORD_SLACK).min(name.len())];
        near.iter()
            .position(|c| c.is_whitespace())
            .map_or(at, |space| at + space + 1)
    } else {
        let from = at.saturating_sub(WORD_SLACK);
        name[from..at]
            .iter()
            .rposition(|c| c.is_whitespace())
            .map_or(at, |space| from + space)
    }
}

// A part of a name, cut at its start when `before`, and at its end when
// `after`, where the end of the name follows, if any. Each cut is an
// ellipsis, without the spaces and dashes that it leaves next to it
fn joined(head: &[char], tail: &[char], before: bool, after: bool) -> String {
    let separator = |c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—' | ':' | ',' | '_');
    let head: String = head.iter().collect();
    let tail: String = tail.iter().collect();
    let mut label = String::new();
    if before {
        label.push('…');
        label.push_str(head.trim_start_matches(separator));
    } else {
        label.push_str(&head);
    }
    if after {
        label.truncate(label.trim_end_matches(separator).len());
        label.push('…');
        label.push_str(tail.trim_start_matches(separator));
    }
    label
}

// The target of the action that opens a booklet: its URI, which is text
// whatever the encoding of its path
pub(crate) fn target(booklet: &Path) -> glib::Variant {
    gio::File::for_path(booklet).uri().to_variant()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use gtk::glib;

    use super::{MAX_BOOKLETS, MAX_LABEL, find, labels};

    fn names(dir: &Path, found: &[PathBuf]) -> Vec<String> {
        found
            .iter()
            .map(|path| {
                path.strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn booklets_are_any_pdf_next_to_the_tracks() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-booklets-XXXXXX")).unwrap();
        for name in [
            "01 Kyrie.flac",
            "liner notes - Messe.pdf",
            "booklet 10.PDF",
            "booklet 2.pdf",
            ".hidden.pdf",
            "notes.txt",
            "cover.jpg",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        std::fs::create_dir(dir.join("folder.pdf")).unwrap();

        let found = find(&[dir.join("01 Kyrie.flac"), dir.join("02 Gloria.flac")]);
        assert_eq!(
            names(&dir, &found),
            ["booklet 2.pdf", "booklet 10.PDF", "liner notes - Messe.pdf"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn labels_of(names: &[&str]) -> Vec<String> {
        labels(&names.iter().map(ToString::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn booklets_are_named_without_the_extension() {
        assert_eq!(
            labels_of(&["booklet.pdf", "Libretto.PDF", "notes.v2.pdf"]),
            ["booklet", "Libretto", "notes.v2"]
        );
    }

    #[test]
    fn long_names_of_booklets_keep_their_start_and_their_end() {
        let labels = labels_of(&[
            "Digital Booklet - Complete Piano Concertos (Paul Lewis) - CD 1.pdf",
            "Digital Booklet - Complete Piano Concertos (Paul Lewis) - CD 2.pdf",
        ]);
        // Between words, and the ends tell them apart
        assert_eq!(
            labels,
            [
                "Digital Booklet…Lewis) - CD 1",
                "Digital Booklet…Lewis) - CD 2"
            ]
        );

        // Without spaces, at the length
        let label = &labels_of(&[&format!("{}.pdf", "x".repeat(60))])[0];
        assert_eq!(label.chars().count(), MAX_LABEL);
    }

    #[test]
    fn long_names_that_differ_in_the_middle_show_it() {
        let labels = labels_of(&[
            "Digital Booklet - Symphony No. 5 in C minor - Complete Recording.pdf",
            "Digital Booklet - Symphony No. 7 in A major - Complete Recording.pdf",
        ]);
        assert_eq!(
            labels,
            [
                "…Symphony No. 5 in C minor - Complete…",
                "…Symphony No. 7 in A major - Complete…"
            ]
        );
        assert!(
            labels
                .iter()
                .all(|label| label.chars().count() <= MAX_LABEL)
        );
    }

    #[test]
    fn names_that_are_still_the_same_are_left_whole() {
        assert_eq!(
            labels_of(&["Booklet.pdf", "Booklet.PDF"]),
            ["Booklet.pdf", "Booklet.PDF"]
        );
        // Long names with the same start, told apart by their ends
        let names = [
            &format!("{} a {}.pdf", "x".repeat(50), "y".repeat(50)),
            &format!("{} a {}.pdf", "x".repeat(50), "z".repeat(50)),
        ];
        let labels = labels_of(&names.map(String::as_str));
        assert_ne!(labels[0], labels[1]);
        assert!(
            labels
                .iter()
                .all(|label| label.chars().count() <= MAX_LABEL)
        );
    }

    #[test]
    fn accents_written_as_marks_stay_with_their_letters() {
        // "Dvořák" with its accents as marks of their own, as from macOS
        let name = format!(
            "{} Booklet of the Symphonies.pdf",
            "Dvor\u{30c}a\u{301}k ".repeat(4)
        );
        let label = &labels_of(&[&name])[0];
        assert!(label.starts_with("Dvořák Dvořák"));
        assert!(!label.contains('\u{301}'));
    }

    #[test]
    fn a_folder_full_of_pdf_files_has_no_booklets() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-booklets-XXXXXX")).unwrap();
        for i in 0..MAX_BOOKLETS {
            std::fs::write(dir.join(format!("paper {i}.pdf")), b"").unwrap();
        }
        let track = dir.join("talk.mp3");
        assert_eq!(find(std::slice::from_ref(&track)).len(), MAX_BOOKLETS);

        std::fs::write(dir.join("one more.pdf"), b"").unwrap();
        assert!(find(&[track]).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn booklets_of_discs_are_in_their_folders_and_the_album_folder() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-booklets-XXXXXX")).unwrap();
        for folder in ["CD 1", "CD 2", "Bonus"] {
            std::fs::create_dir(dir.join(folder)).unwrap();
        }
        for name in ["booklet.pdf", "CD 1/libretto.pdf", "Bonus/other.pdf"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }

        let found = find(&[
            dir.join("CD 1/01.flac"),
            dir.join("CD 2/01.flac"),
            dir.join("CD 2/02.flac"),
        ]);
        assert_eq!(names(&dir, &found), ["booklet.pdf", "CD 1/libretto.pdf"]);

        // A folder that isn't a disc's keeps to itself
        let found = find(&[dir.join("Bonus/01.flac")]);
        assert_eq!(names(&dir, &found), ["Bonus/other.pdf"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn booklets_of_cue_sheet_tracks_are_next_to_the_sheet() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-booklets-XXXXXX")).unwrap();
        for name in ["Album.cue", "Album.flac", "booklet.pdf"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }

        let found = find(&[dir.join("Album.cue/track03")]);
        assert_eq!(names(&dir, &found), ["booklet.pdf"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tracks_whose_folder_cant_be_read_have_no_booklet() {
        assert!(find(&[PathBuf::from("/nonexistent/dismas/01.flac")]).is_empty());
        assert!(find(&[]).is_empty());
    }
}
