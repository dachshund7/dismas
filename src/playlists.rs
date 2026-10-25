// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The playlists, saved as M3U8 files in the user data dir: the queue in
// queue.m3u8, and the others in the playlists directory, one file for
// each, named after the playlist. Functions taking a name take an empty
// one for the queue. A shuffled playlist keeps its order from before, to
// restore it, in a file of the same name in the unshuffled directory

use std::path::{Path, PathBuf};

use gtk::{gio, glib, prelude::*};
use log::{debug, warn};

const EXTENSION: &str = "m3u8";

// Why a name can't be used for a new playlist
#[derive(Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    // Characters that can't be in a file name, or a hidden file
    Invalid,
    // Longer than a file name can be
    TooLong,
    Exists,
}

fn data_dir() -> PathBuf {
    glib::user_data_dir().join("dismas")
}

fn playlists_dir() -> PathBuf {
    data_dir().join("playlists")
}

fn playlist_path(name: &str) -> Option<PathBuf> {
    path_in(&data_dir(), name)
}

fn unshuffled_path(name: &str) -> Option<PathBuf> {
    path_in(&data_dir().join("unshuffled"), name)
}

// The file of a playlist. The queue is outside the directory of the saved
// playlists, so that it isn't listed with them, and can't have the name of
// one. A name that can't be the name of a file of its own has no file:
// names also come from the settings, and from an action that other programs
// can activate over D-Bus, where "../x" or "/x" would reach other files
fn path_in(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        Some(dir.join(format!("queue.{EXTENSION}")))
    } else if is_valid_name(name) {
        Some(dir.join("playlists").join(format!("{name}.{EXTENSION}")))
    } else {
        warn!("Ignoring the playlist name {name:?}");
        None
    }
}

fn invalid_name(name: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("Invalid playlist name {name:?}"),
    )
}

fn invalid_name_error(name: &str) -> glib::Error {
    glib::Error::new(
        gio::IOErrorEnum::InvalidFilename,
        &format!("Invalid playlist name {name:?}"),
    )
}

// The names of the saved playlists, in alphabetical order
pub fn names() -> Vec<String> {
    let mut names = names_in(&playlists_dir());
    names.sort_by_cached_key(|name| name.to_lowercase());
    names
}

// The names of the playlists with a file in a directory
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == EXTENSION))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_string()))
        .filter(|name| is_valid_name(name))
        .collect()
}

pub fn exists(name: &str) -> bool {
    !name.is_empty() && playlist_path(name).is_some_and(|path| path.is_file())
}

// Whether the name, without surrounding spaces, can be used for a new
// playlist; it's also the name of the file
pub fn check_name(name: &str) -> Result<(), NameError> {
    let name = name.trim();
    if name.is_empty() {
        Err(NameError::Empty)
    } else if !is_valid_name(name) {
        Err(NameError::Invalid)
    } else if !fits_file_name(name) {
        Err(NameError::TooLong)
    } else if exists(name) {
        Err(NameError::Exists)
    } else {
        Ok(())
    }
}

fn is_valid_name(name: &str) -> bool {
    !name.starts_with('.') && !name.contains(['/', '\\']) && !name.chars().any(char::is_control)
}

// File names are limited to 255 bytes, and the playlist is written to a
// temporary file first, whose name GLib makes by adding ".XXXXXX" to the
// file's own, e.g. "Mix.m3u8.A1B2C3"
fn fits_file_name(name: &str) -> bool {
    const TEMPORARY_SUFFIX: &str = ".XXXXXX";
    name.len() + ".".len() + EXTENSION.len() + TEMPORARY_SUFFIX.len() <= 255
}

// Creates an empty playlist; the name has been checked with `check_name()`
pub fn create(name: &str) -> Result<(), glib::Error> {
    create_with_tracks(name, &[])
}

// An order left from a playlist of the same name, e.g. deleted outside
// the app, doesn't make the new one shuffled
pub fn create_with_tracks(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    let name = name.trim();
    drop_unshuffled(name);
    write(name, files)
}

// Renames a saved playlist, and the order it had before it was shuffled;
// the new name has been checked with `check_name()`
pub fn rename(name: &str, new_name: &str) -> std::io::Result<()> {
    rename_in(&data_dir(), name, new_name.trim())
}

fn rename_in(dir: &Path, name: &str, new_name: &str) -> std::io::Result<()> {
    let path = |dir: &Path, name: &str| path_in(dir, name).ok_or_else(|| invalid_name(name));
    std::fs::rename(path(dir, name)?, path(dir, new_name)?)?;

    let unshuffled_dir = dir.join("unshuffled");
    let unshuffled = path(&unshuffled_dir, name)?;
    let new_unshuffled = path(&unshuffled_dir, new_name)?;
    let result = if unshuffled.is_file() {
        std::fs::rename(unshuffled, new_unshuffled)
    } else {
        remove_if_exists(&new_unshuffled)
    };
    if let Err(e) = result {
        warn!("Unable to rename the order of the playlist '{name}' before shuffling: {e}");
    }
    Ok(())
}

// Moves a saved playlist to the trash, from where it can be restored; the
// order it had before it was shuffled is gone
pub fn trash(name: &str) -> Result<(), glib::Error> {
    let path = playlist_path(name).ok_or_else(|| invalid_name_error(name))?;
    gio::File::for_path(path).trash(gio::Cancellable::NONE)?;
    drop_unshuffled(name);
    Ok(())
}

// Whether the playlist was shuffled, and keeps the order it had before
pub fn is_shuffled(name: &str) -> bool {
    unshuffled_path(name).is_some_and(|path| path.is_file())
}

// Keeps the order of a playlist before it's shuffled; it's written before
// the shuffled one, so that a crash in between can't lose it
pub fn save_unshuffled(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    let path = unshuffled_path(name).ok_or_else(|| invalid_name_error(name))?;
    write_path(&path, files)
}

// The order the playlist had before it was shuffled, if it was
pub fn load_unshuffled(name: &str) -> Option<Vec<gio::File>> {
    let path = unshuffled_path(name)?;
    if path.is_file() {
        read_m3u(&path)
    } else {
        None
    }
}

// Forgets the order from before shuffling, once it's restored, or when the
// tracks are replaced
pub fn drop_unshuffled(name: &str) {
    let Some(path) = unshuffled_path(name) else {
        return;
    };
    if let Err(e) = remove_if_exists(&path) {
        warn!("Unable to remove the order of the playlist '{name}' before shuffling: {e}");
    }
}

// A playlist made with the tracks of another one is shuffled like it
pub fn copy_unshuffled(name: &str, new_name: &str) {
    match load_unshuffled(name) {
        Some(files) => {
            if let Err(e) = save_unshuffled(new_name, &files) {
                warn!("Unable to copy the order of the playlist '{name}' before shuffling: {e}");
            }
        }
        None => drop_unshuffled(new_name),
    }
}

// Drops the orders of playlists that are gone, e.g. removed outside the
// app; the queue is always there
pub fn retain_unshuffled(names: &[String]) {
    retain_unshuffled_in(&data_dir(), names);
}

fn retain_unshuffled_in(dir: &Path, names: &[String]) {
    let unshuffled_dir = dir.join("unshuffled");
    for name in names_in(&unshuffled_dir.join("playlists")) {
        if !names.contains(&name)
            && let Some(path) = path_in(&unshuffled_dir, &name)
            && let Err(e) = remove_if_exists(&path)
        {
            warn!("Unable to remove the order of the playlist '{name}' before shuffling: {e}");
        }
    }
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

// The tracks of a playlist; the queue has none until it's first saved
pub fn load(name: &str) -> Option<Vec<gio::File>> {
    let path = playlist_path(name)?;
    if name.is_empty() && !path.exists() {
        return Some(Vec::new());
    }
    read_m3u(&path)
}

// The tracks of an M3U playlist anywhere, like one to import
pub fn read_m3u(path: &Path) -> Option<Vec<gio::File>> {
    match std::fs::read(path) {
        Ok(contents) => {
            let dir = path.parent().map(gio::File::for_path)?;
            Some(parse_m3u(&String::from_utf8_lossy(&contents), &dir))
        }
        Err(e) => {
            warn!("Unable to read the playlist {path:?}: {e}");
            None
        }
    }
}

// A name for a new playlist, from the name of a file: without the
// characters a name can't have, and numbered if it's taken
pub fn free_name(name: &str) -> String {
    free_name_among(name, exists)
}

fn free_name_among(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut name: String = name
        .trim()
        .trim_start_matches('.')
        .chars()
        .map(|c| match c {
            '/' | '\\' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    // File names are limited to 255 bytes, which leaves room for a number
    while name.len() + EXTENSION.len() > 240 {
        name.pop();
    }
    if name.trim().is_empty() {
        name = "Playlist".to_string();
    }

    let mut candidate = name.clone();
    let mut n = 2;
    while taken(&candidate) {
        candidate = format!("{name} {n}");
        n += 1;
    }
    candidate
}

// Writes a playlist for other players and devices, as an M3U8 file; the
// tracks in the directory of the file, or under it, have paths relative
// to it, so that the playlist works wherever the directory is copied.
// Returns the number of tracks outside it, which have absolute paths
pub fn export(name: &str, path: &Path) -> Result<usize, glib::Error> {
    let files = load(name).unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("/"));
    let (text, n_outside) = format_m3u(&files, Some(dir));
    glib::file_set_contents(path, text.as_bytes())?;
    Ok(n_outside)
}

pub fn save(name: &str, files: &[gio::File]) {
    match write(name, files) {
        Ok(()) => debug!("Playlist '{name}' saved"),
        Err(e) => warn!("Unable to save the playlist '{name}': {e}"),
    }
}

fn write(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    let path = playlist_path(name).ok_or_else(|| invalid_name_error(name))?;
    write_path(&path, files)
}

fn write_path(path: &Path, files: &[gio::File]) -> Result<(), glib::Error> {
    if let Some(dir) = path.parent() {
        glib::mkdir_with_parents(dir, 0o700);
    }
    // Written to a temporary file first, so that a crash can't leave
    // half a playlist
    glib::file_set_contents(path, format_m3u(files, None).0.as_bytes())
}

// An M3U8 playlist, with the absolute paths of the local files, which
// other players read best, and the URIs of the others; with `relative_to`,
// the files under that directory have relative paths. Returns the number
// of files that aren't under it
fn format_m3u(files: &[gio::File], relative_to: Option<&Path>) -> (String, usize) {
    let mut text = String::from("#EXTM3U\n");
    let mut n_outside = 0;
    for file in files {
        let path = file.path();
        let relative = match (&path, relative_to) {
            (Some(path), Some(dir)) => {
                let relative = path.strip_prefix(dir).ok();
                if relative.is_none() {
                    n_outside += 1;
                }
                relative
            }
            _ => None,
        };
        let line = match relative.or(path.as_deref()).and_then(Path::to_str) {
            // Players take a line starting with "#" for a comment, and drop
            // the spaces in front of a path
            Some(path) if relative.is_some() && path.starts_with(['#', ' ', '\t']) => {
                format!("./{path}")
            }
            Some(path) => path.to_string(),
            None => file.uri().to_string(),
        };
        // A line break would split the entry in two
        if line.contains(['\n', '\r']) {
            warn!("Unable to save {line:?} in a playlist");
            continue;
        }
        text.push_str(&line);
        text.push('\n');
    }
    (text, n_outside)
}

// The entries of an M3U playlist, which can be URIs, or paths; relative
// paths are relative to `base`, the directory of the playlist
fn parse_m3u(text: &str, base: &gio::File) -> Vec<gio::File> {
    text.lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        // GLib can't take a path with a NUL byte, as in a UTF-16 playlist
        // read as UTF-8, or a file with a block of zeros
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.contains('\0'))
        .map(|line| {
            // A path can have "://" in a file name too
            if !line.starts_with('/') && line.contains("://") {
                return gio::File::for_uri(line);
            }
            // Players on Windows, or set to be compatible with it, use
            // backslashes
            let line = if line.contains('/') {
                line.to_string()
            } else {
                line.replace('\\', "/")
            };
            if Path::new(&line).is_absolute() {
                gio::File::for_path(&line)
            } else {
                base.resolve_relative_path(&line)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(files: &[gio::File]) -> Vec<String> {
        files
            .iter()
            .map(|f| f.path().unwrap().to_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn parse_skips_comments_and_blank_lines() {
        let text =
            "\u{feff}#EXTM3U\n#EXTINF:254,Artist - Title\n/music/a.opus\n\n  \n/music/b.flac\r\n";
        let files = parse_m3u(text, &gio::File::for_path("/playlists"));
        assert_eq!(paths(&files), ["/music/a.opus", "/music/b.flac"]);
    }

    #[test]
    fn parse_takes_absolute_paths_as_paths() {
        let text = "/music/Live at Home://Bootleg.flac\n";
        let files = parse_m3u(text, &gio::File::for_path("/playlists"));
        // GIO writes the double slash as one, which names the same file
        assert_eq!(paths(&files), ["/music/Live at Home:/Bootleg.flac"]);
    }

    #[test]
    fn parse_skips_lines_with_nul_bytes() {
        let text = "/music/a.opus\n/music/\0b.flac\n\0\0\0\n/music/c.flac\n";
        let files = parse_m3u(text, &gio::File::for_path("/playlists"));
        assert_eq!(paths(&files), ["/music/a.opus", "/music/c.flac"]);
    }

    #[test]
    fn parse_resolves_relative_paths_and_uris() {
        let text = "Album/01 Track.opus\nfile:///music/caf%C3%A9.opus\n";
        let files = parse_m3u(text, &gio::File::for_path("/home/me/Music"));
        assert_eq!(
            paths(&files),
            ["/home/me/Music/Album/01 Track.opus", "/music/café.opus"]
        );
    }

    #[test]
    fn format_round_trips() {
        let files = [
            gio::File::for_path("/music/Café/01 a.opus"),
            gio::File::for_path("/music/b.flac"),
        ];
        let (text, _) = format_m3u(&files, None);
        assert_eq!(text, "#EXTM3U\n/music/Café/01 a.opus\n/music/b.flac\n");
        let parsed = parse_m3u(&text, &gio::File::for_path("/"));
        assert_eq!(paths(&parsed), paths(&files));
    }

    #[test]
    fn format_skips_line_breaks() {
        let files = [gio::File::for_path("/music/a\nb.opus")];
        assert_eq!(format_m3u(&files, None).0, "#EXTM3U\n");
    }

    #[test]
    fn format_relative_to_a_directory() {
        let files = [
            gio::File::for_path("/home/me/Music/Album/01 a.opus"),
            gio::File::for_path("/home/me/Other/b.flac"),
        ];
        let (text, n_outside) = format_m3u(&files, Some(Path::new("/home/me/Music")));
        assert_eq!(text, "#EXTM3U\nAlbum/01 a.opus\n/home/me/Other/b.flac\n");
        assert_eq!(n_outside, 1);
    }

    #[test]
    fn format_relative_paths_that_look_like_comments() {
        let files = [
            gio::File::for_path("/music/#1 Hits/a.opus"),
            gio::File::for_path("/music/ b.opus"),
        ];
        let (text, _) = format_m3u(&files, Some(Path::new("/music")));
        assert_eq!(text, "#EXTM3U\n./#1 Hits/a.opus\n./ b.opus\n");
        let parsed = parse_m3u(&text, &gio::File::for_path("/music"));
        assert_eq!(paths(&parsed), paths(&files));
    }

    // The tracks of CUE sheets are under their sheet, which is a file
    #[test]
    fn format_round_trips_tracks_of_cue_sheets() {
        let files = [
            gio::File::for_path("/home/me/Music/Album/Album.cue/track03"),
            gio::File::for_path("/home/me/Music/Live.flac/track12"),
        ];
        let (text, n_outside) = format_m3u(&files, Some(Path::new("/home/me/Music")));
        assert_eq!(
            text,
            "#EXTM3U\nAlbum/Album.cue/track03\nLive.flac/track12\n"
        );
        assert_eq!(n_outside, 0);
        let parsed = parse_m3u(&text, &gio::File::for_path("/home/me/Music"));
        assert_eq!(paths(&parsed), paths(&files));
    }

    #[test]
    fn parse_backslashes() {
        let text = "Album\\01 a.opus\n";
        let files = parse_m3u(text, &gio::File::for_path("/music"));
        assert_eq!(paths(&files), ["/music/Album/01 a.opus"]);
    }

    #[test]
    fn free_names() {
        let none = |_: &str| false;
        assert_eq!(free_name_among("Road/trip", none), "Road-trip");
        assert_eq!(free_name_among(".hidden", none), "hidden");
        assert_eq!(free_name_among("  ", none), "Playlist");

        let taken = |name: &str| name == "Mix" || name == "Mix 2";
        assert_eq!(free_name_among("Mix", taken), "Mix 3");
    }

    // A data dir of its own for a test, with a playlist named "Mix" that
    // was shuffled
    fn shuffled_mix() -> PathBuf {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let files = [gio::File::for_path("/music/a.opus")];
        write_path(&path_in(&dir, "Mix").unwrap(), &files).unwrap();
        write_path(&path_in(&dir.join("unshuffled"), "Mix").unwrap(), &files).unwrap();
        dir
    }

    #[test]
    fn the_queue_and_a_playlist_named_queue_have_their_own_files() {
        assert_ne!(playlist_path("").unwrap(), playlist_path("queue").unwrap());
        assert_ne!(
            unshuffled_path("").unwrap(),
            unshuffled_path("queue").unwrap()
        );
        assert_ne!(
            unshuffled_path("Mix").unwrap(),
            playlist_path("Mix").unwrap()
        );
    }

    #[test]
    fn renaming_moves_the_order_before_shuffling() {
        let dir = shuffled_mix();
        let unshuffled_dir = dir.join("unshuffled");
        // Left from a playlist of that name deleted outside the app
        write_path(&path_in(&unshuffled_dir, "Old").unwrap(), &[]).unwrap();

        rename_in(&dir, "Mix", "Road Trip").unwrap();
        assert!(path_in(&dir, "Road Trip").unwrap().is_file());
        assert!(path_in(&unshuffled_dir, "Road Trip").unwrap().is_file());
        assert!(!path_in(&unshuffled_dir, "Mix").unwrap().is_file());

        // Without an order of its own, a stale one under the new name goes
        std::fs::remove_file(path_in(&unshuffled_dir, "Road Trip").unwrap()).unwrap();
        rename_in(&dir, "Road Trip", "Old").unwrap();
        assert!(path_in(&dir, "Old").unwrap().is_file());
        assert!(!path_in(&unshuffled_dir, "Old").unwrap().is_file());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drops_the_orders_of_playlists_that_are_gone() {
        let dir = shuffled_mix();
        let unshuffled_dir = dir.join("unshuffled");
        write_path(&path_in(&unshuffled_dir, "Gone").unwrap(), &[]).unwrap();
        write_path(&path_in(&unshuffled_dir, "").unwrap(), &[]).unwrap();

        retain_unshuffled_in(&dir, &["Mix".to_string()]);
        assert!(path_in(&unshuffled_dir, "Mix").unwrap().is_file());
        assert!(!path_in(&unshuffled_dir, "Gone").unwrap().is_file());
        // The queue is always there
        assert!(path_in(&unshuffled_dir, "").unwrap().is_file());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_that_would_reach_other_files_have_none() {
        let dir = Path::new("/data");
        assert_eq!(path_in(dir, "../x"), None);
        assert_eq!(path_in(dir, "/home/me/x"), None);
        assert_eq!(path_in(dir, "a/b"), None);
        assert_eq!(path_in(dir, ".."), None);
        assert!(!exists("../../x"));
        assert_eq!(load("/home/me/x"), None);
        assert!(save_unshuffled("../x", &[]).is_err());
        assert_eq!(
            path_in(dir, "Mix"),
            Some(PathBuf::from("/data/playlists/Mix.m3u8"))
        );
    }

    #[test]
    fn invalid_names() {
        assert_eq!(check_name("  "), Err(NameError::Empty));
        assert_eq!(check_name(".hidden"), Err(NameError::Invalid));
        assert_eq!(check_name("a/b"), Err(NameError::Invalid));
        assert_eq!(check_name("a\nb"), Err(NameError::Invalid));
        assert_eq!(check_name(&"x".repeat(300)), Err(NameError::TooLong));
        // Limited in bytes, which characters can take several of
        assert_eq!(check_name(&"é".repeat(122)), Err(NameError::TooLong));
        // Room for ".m3u8" and the ".XXXXXX" of the temporary file
        assert_eq!(check_name(&"x".repeat(243)), Ok(()));
        assert_eq!(check_name(&"x".repeat(244)), Err(NameError::TooLong));
        assert!(is_valid_name("Road trip: 2026 (best)"));
    }
}
