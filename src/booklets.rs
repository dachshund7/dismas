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
pub const OPEN_ACTION: &str = "win.open-booklet";

// The PDF files in the folders of the tracks at `paths`, and in the folder
// of their album when they're in a disc folder, as in Album/CD1, where the
// album's booklet is often left; in the order a file manager shows their
// names. This reads the folders, so it must not run on the main thread
pub fn find(paths: &[PathBuf]) -> Vec<PathBuf> {
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
    entries
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
        .collect()
}

// The item of a menu that opens the booklets: "Open Booklet" for one, and
// for several, a submenu of their names
pub fn menu_item(booklets: &[PathBuf]) -> Option<gio::MenuItem> {
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

// A menu of the booklets, by their file names
pub fn menu(booklets: &[PathBuf]) -> gio::Menu {
    let menu = gio::Menu::new();
    for booklet in booklets {
        let name = booklet.file_name().unwrap_or_default().to_string_lossy();
        // An underscore would underline the letter after it
        let item = gio::MenuItem::new(Some(&name.replace('_', "__")), None);
        item.set_action_and_target_value(Some(OPEN_ACTION), Some(&target(booklet)));
        menu.append_item(&item);
    }
    menu
}

// The target of the action that opens a booklet: its URI, which is text
// whatever the encoding of its path
pub fn target(booklet: &Path) -> glib::Variant {
    gio::File::for_path(booklet).uri().to_variant()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use gtk::glib;

    use super::find;

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
