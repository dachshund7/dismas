// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The playlists, saved as M3U8 files in the user data dir: the queue in
// queue.m3u8, and the others in the playlists directory, one file for
// each, named after the playlist. Functions taking a name take an empty
// one for the queue

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
    Exists,
}

fn data_dir() -> PathBuf {
    glib::user_data_dir().join("amberol")
}

fn playlists_dir() -> PathBuf {
    data_dir().join("playlists")
}

// The queue is outside the directory of the saved playlists, so that it
// isn't listed with them, and can't have the name of one
fn playlist_path(name: &str) -> PathBuf {
    if name.is_empty() {
        data_dir().join(format!("queue.{EXTENSION}"))
    } else {
        playlists_dir().join(format!("{name}.{EXTENSION}"))
    }
}

// The names of the saved playlists, in alphabetical order
pub fn names() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(playlists_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == EXTENSION))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_string()))
        .collect();
    names.sort_by_cached_key(|name| name.to_lowercase());
    names
}

pub fn exists(name: &str) -> bool {
    !name.is_empty() && playlist_path(name).is_file()
}

// Whether the name, without surrounding spaces, can be used for a new
// playlist; it's also the name of the file
pub fn check_name(name: &str) -> Result<(), NameError> {
    let name = name.trim();
    if name.is_empty() {
        Err(NameError::Empty)
    } else if !is_valid_name(name) {
        Err(NameError::Invalid)
    } else if exists(name) {
        Err(NameError::Exists)
    } else {
        Ok(())
    }
}

fn is_valid_name(name: &str) -> bool {
    !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
        // File names are limited to 255 bytes
        && name.len() + EXTENSION.len() < 255
}

// Creates an empty playlist; the name has been checked with `check_name()`
pub fn create(name: &str) -> Result<(), glib::Error> {
    write(name.trim(), &[])
}

// The tracks of a playlist; the queue has none until it's first saved
pub fn load(name: &str) -> Option<Vec<gio::File>> {
    let path = playlist_path(name);
    match std::fs::read(&path) {
        Ok(contents) => {
            let dir = path.parent().map(gio::File::for_path)?;
            Some(parse_m3u(&String::from_utf8_lossy(&contents), &dir))
        }
        Err(e) if name.is_empty() && e.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(e) => {
            warn!("Unable to read the playlist {path:?}: {e}");
            None
        }
    }
}

pub fn save(name: &str, files: &[gio::File]) {
    match write(name, files) {
        Ok(()) => debug!("Playlist '{name}' saved"),
        Err(e) => warn!("Unable to save the playlist '{name}': {e}"),
    }
}

fn write(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    let path = playlist_path(name);
    if let Some(dir) = path.parent() {
        glib::mkdir_with_parents(dir, 0o755);
    }
    // Written to a temporary file first, so that a crash can't leave
    // half a playlist
    glib::file_set_contents(path, format_m3u(files).as_bytes())
}

// An M3U8 playlist, with the absolute paths of the local files, which
// other players read best, and the URIs of the others
fn format_m3u(files: &[gio::File]) -> String {
    let mut text = String::from("#EXTM3U\n");
    for file in files {
        let line = match file.path() {
            Some(path) => match path.to_str() {
                Some(path) => path.to_string(),
                None => file.uri().to_string(),
            },
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
    text
}

// The entries of an M3U playlist, which can be URIs, or paths; relative
// paths are relative to `base`, the directory of the playlist
fn parse_m3u(text: &str, base: &gio::File) -> Vec<gio::File> {
    text.lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            if line.contains("://") {
                gio::File::for_uri(line)
            } else if Path::new(line).is_absolute() {
                gio::File::for_path(line)
            } else {
                base.resolve_relative_path(line)
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
        let text = format_m3u(&files);
        assert_eq!(text, "#EXTM3U\n/music/Café/01 a.opus\n/music/b.flac\n");
        let parsed = parse_m3u(&text, &gio::File::for_path("/"));
        assert_eq!(paths(&parsed), paths(&files));
    }

    #[test]
    fn format_skips_line_breaks() {
        let files = [gio::File::for_path("/music/a\nb.opus")];
        assert_eq!(format_m3u(&files), "#EXTM3U\n");
    }

    #[test]
    fn invalid_names() {
        assert_eq!(check_name("  "), Err(NameError::Empty));
        assert_eq!(check_name(".hidden"), Err(NameError::Invalid));
        assert_eq!(check_name("a/b"), Err(NameError::Invalid));
        assert_eq!(check_name("a\nb"), Err(NameError::Invalid));
        assert_eq!(check_name(&"x".repeat(300)), Err(NameError::Invalid));
        assert!(is_valid_name("Road trip: 2026 (best)"));
    }
}
