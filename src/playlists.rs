// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The playlists, saved as M3U8 files in the user data dir: the queue in
// queue.m3u8, and the others in the playlists directory, one file for
// each, named after the playlist. Functions taking a name take an empty
// one for the queue. A shuffled playlist keeps its order from before, to
// restore it, in a file of the same name in the unshuffled directory

use std::{
    borrow::Cow,
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
};

use gtk::{gio, glib, prelude::*};
use log::{debug, warn};

use crate::{cue_sheet, utils};

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
    load_in(&data_dir(), name)
}

fn load_in(dir: &Path, name: &str) -> Option<Vec<gio::File>> {
    let path = path_in(dir, name)?;
    if name.is_empty() && !path.exists() {
        return Some(Vec::new());
    }
    read_m3u(&path)
}

// Adds tracks at the end of a playlist that isn't selected, whose tracks
// are only in its file; the selected one is saved from the queue, which
// would write over them. Its point to resume from and its order from
// before shuffling stay right, as the tracks before them don't move
pub fn append(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    append_in(&data_dir(), name, files)
}

fn append_in(dir: &Path, name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    let path = path_in(dir, name).ok_or_else(|| invalid_name_error(name))?;
    let mut tracks = load_in(dir, name).ok_or_else(|| {
        glib::Error::new(
            gio::IOErrorEnum::Failed,
            &format!("Unable to read the playlist {name:?}"),
        )
    })?;
    tracks.extend_from_slice(files);
    write_path(&path, &tracks)
}

// The tracks of an M3U playlist anywhere. Playlists written on Windows
// are often in its legacy encoding, which is guessed as for CUE sheets
pub fn read_m3u(path: &Path) -> Option<Vec<gio::File>> {
    match std::fs::read(path) {
        Ok(contents) => {
            let dir = path.parent().map(gio::File::for_path)?;
            Some(parse_m3u(&cue_sheet::decode(&contents), &dir))
        }
        Err(e) => {
            warn!("Unable to read the playlist {path:?}: {e}");
            None
        }
    }
}

// The largest playlist from elsewhere that's read, and the most tracks it
// can name. Dismas writes a path a line, about 100 bytes, and other players
// add an #EXTINF line to each, so 32 MiB is 170,000 tracks or more, while
// a file of short lines, such as "a", would make 16 million entries, each
// a `gio::File` and then a track. Dismas's own playlists aren't limited, as
// one that couldn't be read would be saved again without its tracks
const MAX_PLAYLIST_SIZE: u64 = 32 * 1024 * 1024;
const MAX_PLAYLIST_ENTRIES: usize = 200_000;

// The tracks of an M3U playlist from elsewhere, like one to import: an
// entry that isn't where the playlist says is looked for where it can be,
// as the playlist can come from another computer, or from Windows, see
// `Finder`; the entries that aren't found are left as they are. A playlist
// over the limits isn't read at all, rather than read in part
pub fn import_m3u(path: &Path, music_folders: &[PathBuf]) -> Option<Vec<gio::File>> {
    import_m3u_within(path, music_folders, MAX_PLAYLIST_SIZE, MAX_PLAYLIST_ENTRIES)
}

fn import_m3u_within(
    path: &Path,
    music_folders: &[PathBuf],
    max_size: u64,
    max_entries: usize,
) -> Option<Vec<gio::File>> {
    let mut contents = Vec::new();
    // Not a named pipe, which would wait for something to write to it
    let read = utils::open_regular_file(path)
        .and_then(|file| file.take(max_size + 1).read_to_end(&mut contents));
    if let Err(e) = read {
        warn!("Unable to read the playlist {path:?}: {e}");
        return None;
    }
    if contents.len() as u64 > max_size {
        warn!("Not reading the playlist {path:?}, which is larger than {max_size} bytes");
        return None;
    }
    let text = cue_sheet::decode(&contents);
    if m3u_entries(&text).nth(max_entries).is_some() {
        warn!("Not reading the playlist {path:?}, which names more than {max_entries} tracks");
        return None;
    }
    let dir = path.parent()?;
    let base = gio::File::for_path(dir);
    let mut finder = Finder::default();
    Some(
        m3u_entries(&text)
            .map(|line| {
                let file = m3u_entry(line, &base);
                if file.path().is_some_and(|p| cue_sheet::location_exists(&p)) {
                    return file;
                }
                finder
                    .find(line, dir, music_folders)
                    .map_or(file, gio::File::for_path)
            })
            .collect(),
    )
}

// Looks for the tracks of a playlist from elsewhere: a path that differs
// in case, as one written on Windows can, or the end of an absolute path,
// as in "C:\Users\…\Music\Artist\Album\01.flac", in the folder of
// the playlist and then in the music folders, where the end must have a
// folder and a file, as a file name alone could be found anywhere. The
// folders read are kept, as the tracks of a playlist share them
// The longest path the system opens (Linux's PATH_MAX, with the NUL that
// ends it)
const MAX_PATH_LEN: usize = 4095;

// How many of the parts at the end of an absolute path are tried, the file
// and the folders above it: more than a real path has, while each one
// tried costs a few looks at the disk, in the playlist's folder and in
// each music folder, and a crafted line can name millions
const MAX_TRIED_PARTS: usize = 32;

// The end of `path` that's at most `max` bytes, from the first part that
// starts in it
fn path_end(path: &str, max: usize) -> &str {
    if path.len() <= max {
        return path;
    }
    let start = path.ceil_char_boundary(path.len() - max);
    let end = &path[start..];
    if path[..start].ends_with(['/', '\\']) {
        end
    } else {
        end.find(['/', '\\']).map_or("", |i| &end[i..])
    }
}

#[derive(Default)]
struct Finder {
    listings: HashMap<PathBuf, Vec<(String, PathBuf)>>,
}

impl Finder {
    fn find(&mut self, line: &str, dir: &Path, music_folders: &[PathBuf]) -> Option<PathBuf> {
        let (absolute, path) = if let Some(uri) = line.strip_prefix("file://") {
            (
                true,
                glib::Uri::unescape_string(uri, None::<&str>)?.to_string(),
            )
        } else if line.contains("://") {
            return None;
        } else {
            // A drive, as in "C:\", or a network share, as in "\\server\"
            let windows = line.as_bytes().get(1) == Some(&b':') || line.starts_with("\\\\");
            (windows || line.starts_with('/'), line.to_string())
        };
        // A longer path names no file, so only the end of an absolute one
        // can be found
        if !absolute && path.len() > MAX_PATH_LEN {
            return None;
        }
        let parts: Vec<&str> = path_end(&path, MAX_PATH_LEN)
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if parts.is_empty() || parts.contains(&"..") {
            return None;
        }
        if !absolute {
            return self.find_in(dir, &parts);
        }
        let parts = &parts[parts.len().saturating_sub(MAX_TRIED_PARTS)..];
        (0..parts.len()).find_map(|start| {
            let end = &parts[start..];
            self.find_in(dir, end).or_else(|| {
                (end.len() >= 2)
                    .then(|| {
                        music_folders
                            .iter()
                            .find_map(|folder| self.find_in(folder, end))
                    })
                    .flatten()
            })
        })
    }

    // The path of `parts` in `dir`, matching a part ignoring case when it
    // isn't there as it's written, if one entry of its folder matches it.
    // A track of a CUE sheet is a path under the sheet, which is a file
    fn find_in(&mut self, dir: &Path, parts: &[&str]) -> Option<PathBuf> {
        let mut path = dir.to_path_buf();
        for (i, part) in parts.iter().enumerate() {
            if path.is_file() {
                path.extend(&parts[i..]);
                break;
            }
            let exact = path.join(part);
            if exact.exists() {
                path = exact;
                continue;
            }
            let listing = self.listings.entry(path.clone()).or_insert_with(|| {
                std::fs::read_dir(&path)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| {
                        (
                            entry.file_name().to_string_lossy().to_lowercase(),
                            entry.path(),
                        )
                    })
                    .collect()
            });
            let lower = part.to_lowercase();
            let mut matching = listing.iter().filter(|(name, _)| *name == lower);
            let (_, found) = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            path.clone_from(found);
        }
        cue_sheet::location_exists(&path).then_some(path)
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
    // The playlist is made with the name trimmed, so it's checked trimmed:
    // otherwise ". Mix" would give " Mix", which looks free, and the
    // playlist made would write over "Mix"
    name = name.trim().to_string();
    if name.is_empty() {
        name = "Playlist".to_string();
    }

    if !taken(&name) {
        return name;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{name} {n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
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
        let line: Cow<str> = match relative.or(path.as_deref()).and_then(Path::to_str) {
            // Players take a line starting with "#" for a comment, and drop
            // the spaces in front of a path
            Some(path) if relative.is_some() && path.starts_with(['#', ' ', '\t']) => {
                format!("./{path}").into()
            }
            Some(path) => path.into(),
            None => String::from(file.uri()).into(),
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
    m3u_entries(text)
        .map(|line| m3u_entry(line, base))
        .collect()
}

// The lines of an M3U playlist that name tracks. Old Macs ended lines
// with a carriage return alone
fn m3u_entries(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\r'])
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        // GLib can't take a path with a NUL byte, as in a UTF-16 playlist
        // without a byte order mark, or a file with a block of zeros
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.contains('\0'))
}

fn m3u_entry(line: &str, base: &gio::File) -> gio::File {
    // A path can have "://" in a file name too
    if !line.starts_with('/') && line.contains("://") {
        return gio::File::for_uri(line);
    }
    // Players on Windows, or set to be compatible with it, use
    // backslashes
    let line: Cow<str> = if line.contains('/') {
        line.into()
    } else {
        line.replace('\\', "/").into()
    };
    if Path::new(&*line).is_absolute() {
        gio::File::for_path(&*line)
    } else {
        base.resolve_relative_path(&*line)
    }
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

    #[test]
    fn free_names_are_checked_trimmed() {
        // A name that only a trim makes the same as a playlist's is taken
        let taken = |name: &str| name == "Road Trip";
        for stem in [". Road Trip", "..  Road Trip", "Road Trip\u{7f}"] {
            assert_eq!(free_name_among(stem, taken), "Road Trip 2", "{stem:?}");
        }

        // The name checked is always the name the playlist is made with,
        // also when the limit on its length cuts it after a space
        let cut_after_space = format!("{} b", "a".repeat(235));
        assert_eq!(
            free_name_among(&cut_after_space, |_| false),
            "a".repeat(235)
        );
        for stem in [". Mix", "Mix\t", "\u{1}Mix", &cut_after_space] {
            let name = free_name_among(stem, |_| false);
            assert_eq!(name, name.trim(), "{stem:?}");
            let numbered = free_name_among(stem, |n| n == name);
            assert_eq!(numbered, numbered.trim(), "{stem:?}");
        }
    }

    #[test]
    fn append_adds_tracks_at_the_end() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let file = |name: &str| gio::File::for_path(format!("/music/{name}"));
        let paths = |name: &str| -> Vec<_> {
            load_in(&dir, name)
                .unwrap()
                .iter()
                .map(|f| f.path().unwrap())
                .collect()
        };

        // The queue has no file until it's saved
        append_in(&dir, "", &[file("a.opus")]).unwrap();
        assert_eq!(paths(""), [Path::new("/music/a.opus")]);

        write_path(&path_in(&dir, "Mix").unwrap(), &[file("b.opus")]).unwrap();
        append_in(&dir, "Mix", &[file("c.opus"), file("Album.cue/track02")]).unwrap();
        assert_eq!(
            paths("Mix"),
            [
                Path::new("/music/b.opus"),
                Path::new("/music/c.opus"),
                Path::new("/music/Album.cue/track02"),
            ]
        );

        // A saved playlist whose file is gone isn't made again
        assert!(append_in(&dir, "Gone", &[file("d.opus")]).is_err());
        assert!(!path_in(&dir, "Gone").unwrap().exists());
        assert!(append_in(&dir, "../x", &[file("d.opus")]).is_err());

        std::fs::remove_dir_all(dir).unwrap();
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

    #[test]
    fn playlists_in_legacy_encodings_and_old_line_endings_are_read() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        // Windows-1252, as Winamp and foobar2000 write .m3u files, with the
        // line endings of old Macs
        let playlist = dir.join("list.m3u");
        std::fs::write(
            &playlist,
            b"#EXTM3U\r/music/Caf\xe9.flac\r/music/B\xe4r.flac\r",
        )
        .unwrap();
        let files = read_m3u(&playlist).unwrap();
        assert_eq!(paths(&files), ["/music/Café.flac", "/music/Bär.flac"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_playlist_from_elsewhere_over_the_limits_is_not_read() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let playlist = dir.join("list.m3u");
        // At most 20 bytes and 2 tracks
        let read = |text: &str| {
            std::fs::write(&playlist, text).unwrap();
            import_m3u_within(&playlist, &[], 20, 2).map(|files| files.len())
        };

        // Comments aren't tracks
        assert_eq!(read("#EXTM3U\n#x\na\nb\n"), Some(2));
        assert_eq!(read("a\nb\nc\n"), None);
        // 20 bytes, then 21
        assert_eq!(read(&format!("#{}\na\n", "x".repeat(16))), Some(1));
        assert_eq!(read(&format!("#{}\na\n", "x".repeat(17))), None);

        // A named pipe isn't waited on
        let pipe = dir.join("pipe.m3u");
        utils::make_test_pipe(&pipe);
        let music_folders = [dir.clone()];
        assert!(utils::within_seconds(move || import_m3u(
            &pipe,
            &music_folders
        )
        .is_none()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn imported_tracks_are_found_where_they_moved() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let music = dir.join("Music");
        std::fs::create_dir_all(music.join("Artist/Album")).unwrap();
        std::fs::write(music.join("Artist/Album/01 One.flac"), b"").unwrap();
        std::fs::write(music.join("Loose.flac"), b"").unwrap();
        let track = music.join("Artist/Album/01 One.flac");
        let elsewhere = dir.join("Elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();

        let import = |folder: &Path, lines: &[&str]| -> Vec<PathBuf> {
            let playlist = folder.join("list.m3u");
            std::fs::write(&playlist, lines.join("\n")).unwrap();
            import_m3u(&playlist, std::slice::from_ref(&music))
                .unwrap()
                .iter()
                .map(|file| file.path().unwrap())
                .collect()
        };

        // The end of a path from Windows or from another computer, in the
        // folder of the playlist, or in the music folders
        for line in [
            "C:\\Users\\me\\Music\\Artist\\Album\\01 One.flac",
            "\\\\server\\music\\Artist\\Album\\01 One.flac",
            "file:///C:/Users/me/Music/Artist/Album/01%20One.flac",
            "/home/someone/Music/Artist/Album/01 One.flac",
        ] {
            assert_eq!(
                import(&music, &[line]),
                std::slice::from_ref(&track),
                "{line}"
            );
            assert_eq!(
                import(&elsewhere, &[line]),
                std::slice::from_ref(&track),
                "{line}"
            );
        }
        // A path written in another case, as on Windows
        assert_eq!(
            import(&music, &["artist/ALBUM/01 one.FLAC"]),
            std::slice::from_ref(&track)
        );
        // A file name alone is only looked for next to the playlist
        assert_eq!(
            import(&music, &["D:\\Loose.flac"]),
            [music.join("Loose.flac")]
        );
        let missing = import(&elsewhere, &["D:\\Loose.flac"]);
        assert_ne!(missing, [music.join("Loose.flac")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_path_of_millions_of_folders_is_looked_for_by_its_end() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let music = dir.join("Music");
        std::fs::create_dir_all(music.join("Artist/Album")).unwrap();
        let track = music.join("Artist/Album/01 One.flac");
        std::fs::write(&track, b"").unwrap();
        let import = |line: String| {
            let playlist = dir.join("list.m3u");
            std::fs::write(&playlist, line).unwrap();
            let music_folders = [music.clone()];
            utils::within_seconds(move || import_m3u(&playlist, &music_folders))
                .unwrap()
                .iter()
                .map(|file| file.path().unwrap())
                .collect::<Vec<_>>()
        };

        // Each folder of the path would be tried as the start of its end,
        // in the playlist's folder and in the music folder, and the track
        // at its end is still found
        let folders = "/a".repeat(1_000_000);
        assert_eq!(
            import(format!("{folders}/Artist/Album/01 One.flac")),
            [track]
        );
        // A relative path longer than the system opens names no file
        let line = format!("{}/Artist/Album/01 One.flac", &folders[1..]);
        assert_eq!(import(line.clone()), [dir.join(line)]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_end_of_a_long_path_starts_with_a_whole_part() {
        assert_eq!(path_end("/ab/cd/ef", 9), "/ab/cd/ef");
        // The cut falls after a separator
        assert_eq!(path_end("/ab/cd/ef", 5), "cd/ef");
        // Inside a part, which is left out
        assert_eq!(path_end("/ab/cd/ef", 4), "/ef");
        // Inside a character
        assert_eq!(path_end("/é/x", 3), "/x");
        assert_eq!(path_end("/abcdef", 3), "");
    }
}
