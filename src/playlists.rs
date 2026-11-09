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

use crate::{
    cue_sheet,
    library::{self, MusicFolder},
    utils,
};

const EXTENSION: &str = "m3u8";

// Why a name can't be used for a new playlist
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NameError {
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
pub(crate) fn names() -> Vec<String> {
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

pub(crate) fn exists(name: &str) -> bool {
    !name.is_empty() && playlist_path(name).is_some_and(|path| path.is_file())
}

// Whether the name, without surrounding spaces, can be used for a new
// playlist; it's also the name of the file
pub(crate) fn check_name(name: &str) -> Result<(), NameError> {
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
pub(crate) fn create(name: &str) -> Result<(), glib::Error> {
    create_with_tracks(name, &[])
}

// An order left from a playlist of the same name, e.g. deleted outside
// the app, doesn't make the new one shuffled
pub(crate) fn create_with_tracks(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    let name = name.trim();
    drop_unshuffled(name);
    write(name, files)
}

// Renames a saved playlist, and the order it had before it was shuffled;
// the new name has been checked with `check_name()`
pub(crate) fn rename(name: &str, new_name: &str) -> std::io::Result<()> {
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
pub(crate) fn trash(name: &str) -> Result<(), glib::Error> {
    let path = playlist_path(name).ok_or_else(|| invalid_name_error(name))?;
    gio::File::for_path(path).trash(gio::Cancellable::NONE)?;
    drop_unshuffled(name);
    Ok(())
}

// Whether the playlist was shuffled, and keeps the order it had before
pub(crate) fn is_shuffled(name: &str) -> bool {
    unshuffled_path(name).is_some_and(|path| path.is_file())
}

// Keeps the order of a playlist before it's shuffled; it's written before
// the shuffled one, so that a crash in between can't lose it
pub(crate) fn save_unshuffled(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
    let path = unshuffled_path(name).ok_or_else(|| invalid_name_error(name))?;
    write_path(&path, files)
}

// The order the playlist had before it was shuffled, if it was
pub(crate) fn load_unshuffled(name: &str) -> Option<Vec<gio::File>> {
    let path = unshuffled_path(name)?;
    if path.is_file() {
        read_m3u(&path)
    } else {
        None
    }
}

// Forgets the order from before shuffling, once it's restored, or when the
// tracks are replaced
pub(crate) fn drop_unshuffled(name: &str) {
    let Some(path) = unshuffled_path(name) else {
        return;
    };
    if let Err(e) = remove_if_exists(&path) {
        warn!("Unable to remove the order of the playlist '{name}' before shuffling: {e}");
    }
}

// A playlist made with the tracks of another one is shuffled like it
pub(crate) fn copy_unshuffled(name: &str, new_name: &str) {
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
pub(crate) fn retain_unshuffled(names: &[String]) {
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
pub(crate) fn load(name: &str) -> Option<Vec<gio::File>> {
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
pub(crate) fn append(name: &str, files: &[gio::File]) -> Result<(), glib::Error> {
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
pub(crate) fn read_m3u(path: &Path) -> Option<Vec<gio::File>> {
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
pub(crate) fn import_m3u(path: &Path, music_folders: &[PathBuf]) -> Option<Vec<gio::File>> {
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
    let mut sheets = cue_sheet::FolderSheets::default();
    Some(
        m3u_entries(&text)
            .map(|entry| {
                let file = m3u_entry(entry.line, &base);
                let file = if file.path().is_some_and(|p| cue_sheet::location_exists(&p)) {
                    file
                } else {
                    finder
                        .find(entry.line, Some(dir), music_folders)
                        .map_or(file, gio::File::for_path)
                };
                with_cue_track(file, entry.cue_track, &mut sheets)
            })
            .collect(),
    )
}

// Looks for the tracks of a playlist from elsewhere: a path that differs
// in case, as one written on Windows can, or the end of an absolute path,
// as in "C:\Users\…\Music\Artist\Album\01.flac", in the folder of
// the playlist and then in the music folders, where the end must have a
// folder and a file, as a file name alone could be found anywhere. Names
// are compared in one Unicode form too, as macOS keeps names decomposed
// and the playlists that Apple Music writes have them composed. The
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
    // Without the playlist's folder, as for a playlist of Dismas's own,
    // only the music folders are looked in
    fn find(
        &mut self,
        line: &str,
        dir: Option<&Path>,
        music_folders: &[PathBuf],
    ) -> Option<PathBuf> {
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
        // A track of a CUE sheet is looked for as its sheet, so that the
        // sheet alone doesn't count as a folder and a file
        let mut parts = parts;
        let cue_track = cue_sheet::missing_location_parts(Path::new(&parts.join("/")))
            .map(|(_, number)| number);
        if cue_track.is_some() {
            parts.pop();
        }
        let found = if absolute {
            let parts = &parts[parts.len().saturating_sub(MAX_TRIED_PARTS)..];
            (0..parts.len()).find_map(|start| {
                let end = &parts[start..];
                dir.and_then(|dir| self.find_in(dir, end)).or_else(|| {
                    (end.len() >= 2)
                        .then(|| {
                            music_folders
                                .iter()
                                .find_map(|folder| self.find_in(folder, end))
                        })
                        .flatten()
                })
            })
        } else {
            self.find_in(dir?, &parts)
        }?;
        Some(match cue_track {
            Some(number) => cue_sheet::track_location(&found, number),
            None => found,
        })
    }

    // The path of `parts` in `dir`, matching a part ignoring case and
    // Unicode form when it isn't there as it's written, if one entry of its
    // folder matches it
    fn find_in(&mut self, dir: &Path, parts: &[&str]) -> Option<PathBuf> {
        let mut path = dir.to_path_buf();
        for part in parts {
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
                    .map(|entry| (folded(&entry.file_name().to_string_lossy()), entry.path()))
                    .collect()
            });
            let folded = folded(part);
            let mut matching = listing.iter().filter(|(name, _)| *name == folded);
            let (_, found) = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            path.clone_from(found);
        }
        cue_sheet::location_exists(&path).then_some(path)
    }
}

// A name as the Finder compares it: composed, as most systems keep names,
// and ignoring case
fn folded(name: &str) -> String {
    glib::normalize(name, glib::NormalizeMode::DefaultCompose).to_lowercase()
}

// A name for a new playlist, from the name of a file: without the
// characters a name can't have, and numbered if it's taken
pub(crate) fn free_name(name: &str) -> String {
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

// What an export wrote that other players read only in part: the tracks
// with absolute paths, outside the playlist's music folder, which other
// devices won't find, unless the user asked for absolute paths, and the
// tracks of CUE sheets, which most players other than Strawberry play as
// their whole audio file
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ExportReport {
    pub(crate) n_absolute: usize,
    pub(crate) n_cue: usize,
}

// What exports go by, from the settings
#[derive(Debug)]
pub(crate) struct ExportOptions {
    pub(crate) music_folders: Vec<MusicFolder>,
    // The path each music folder has in exported playlists, by the path
    // it's read at, see `library::export_mappings()`
    pub(crate) mappings: HashMap<PathBuf, String>,
    pub(crate) absolute: bool,
}

impl ExportOptions {
    pub(crate) fn from_settings(settings: &gio::Settings) -> Self {
        Self {
            music_folders: library::music_folders(settings),
            mappings: library::export_mappings(settings),
            absolute: settings.boolean("export-absolute-paths"),
        }
    }
}

// Writes a playlist for other players and devices, as an M3U8 file, see
// `ExportPaths`. This blocks
pub(crate) fn export(
    name: &str,
    path: &Path,
    options: &ExportOptions,
) -> Result<ExportReport, glib::Error> {
    let files = load(name).unwrap_or_default();
    let mut paths = ExportPaths::new(export_dir(path), options);
    let (text, report) = format_export(&files, &mut paths);
    glib::file_set_contents(path, text.as_bytes())?;
    if paths.dir.is_some() {
        return Ok(report);
    }
    // The document portal may only say where a file it saves is once
    // the file is there
    paths.dir = export_dir(path);
    if paths.dir.is_none() {
        return Ok(report);
    }
    let (text, report) = format_export(&files, &mut paths);
    glib::file_set_contents(path, text.as_bytes())?;
    Ok(report)
}

// The folder of an exported playlist, outside the sandbox and without
// symbolic links, as `library::folder_key()` gives the music folders, to
// make the paths relative to; the document portal gives its files paths of
// their own. This blocks
fn export_dir(file: &Path) -> Option<PathBuf> {
    let dir = file.parent()?;
    if dir.starts_with(glib::user_runtime_dir().join("doc")) {
        return library::host_path(file)?.parent().map(Path::to_path_buf);
    }
    Some(std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()))
}

// How an export names its tracks. Phones and servers such as Navidrome
// read a playlist in the music folder, in a Playlists folder or with the
// albums, by paths relative to it, which climb out of its folder with
// "../", so that the playlist works wherever the folder is copied or
// mounted. So a track has a relative path when it's under the playlist's
// folder, or in the same music folder; otherwise it has an absolute path,
// outside the sandbox, as do all of them when the user asks, as Plex and
// Jellyfin need. An absolute path can start with the path the user gave
// its music folder in place of its own, as the folder is mounted in a
// server's container, or found on a phone or on Windows. Paths are
// compared outside the sandbox and without symbolic links, so that a
// music folder that's a link, or one from the portal, compares as the
// folder it is
struct ExportPaths {
    dir: Option<PathBuf>,
    folders: Vec<ExportFolder>,
    absolute: bool,
    // The folders of the tracks outside the music folders, without their
    // symbolic links, as the tracks of an album share one
    resolved: HashMap<PathBuf, PathBuf>,
}

struct ExportFolder {
    // As the tracks in it are read, as they're written in absolute paths,
    // outside the sandbox or as the user gave it, and as they're compared
    read: PathBuf,
    written: PathBuf,
    mapped: Option<String>,
    key: PathBuf,
}

impl ExportPaths {
    // This blocks
    fn new(dir: Option<PathBuf>, options: &ExportOptions) -> Self {
        let folders = options
            .music_folders
            .iter()
            .map(|folder| ExportFolder {
                read: folder.path.clone(),
                written: folder.shown_path().to_path_buf(),
                mapped: options.mappings.get(&folder.path).cloned(),
                key: library::folder_key(folder),
            })
            .collect();
        Self {
            dir,
            folders,
            absolute: options.absolute,
            resolved: HashMap::new(),
        }
    }

    // The line of a track, and whether it's relative, or None for one with
    // a line break, which would split the entry in two
    fn line(&mut self, path: &Path) -> Option<(String, bool)> {
        let folder = self
            .folders
            .iter()
            .filter(|folder| path.starts_with(&folder.read))
            .max_by_key(|folder| folder.read.components().count());
        let rest = folder.and_then(|folder| Some((folder, path.strip_prefix(&folder.read).ok()?)));
        let key = match rest {
            Some((folder, rest)) => folder.key.join(rest),
            None => resolved_path(&mut self.resolved, path),
        };
        let relative = self
            .dir
            .as_deref()
            .filter(|dir| {
                !self.absolute
                    && (key.starts_with(dir)
                        || self.folders.iter().any(|folder| {
                            dir.starts_with(&folder.key) && key.starts_with(&folder.key)
                        }))
            })
            .map(|dir| relative_path(dir, &key));
        let (line, relative) = match relative.as_deref().and_then(Path::to_str) {
            // Players take a line starting with "#" for a comment, and drop
            // the spaces in front of a path
            Some(relative) if relative.starts_with(['#', ' ', '\t']) => {
                (format!("./{relative}"), true)
            }
            Some(relative) => (relative.to_string(), true),
            None => (absolute_line(path, rest), false),
        };
        if line.contains(['\n', '\r']) {
            warn!("Unable to save {line:?} in a playlist");
            return None;
        }
        Some((line, relative))
    }
}

// The path of a file without the symbolic links of its folder; the
// folders resolved are kept, as the tracks of an album share one
fn resolved_path(resolved: &mut HashMap<PathBuf, PathBuf>, path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolved
            .entry(parent.to_path_buf())
            .or_insert_with(|| {
                std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf())
            })
            .join(name),
        _ => path.to_path_buf(),
    }
}

// The absolute line of a track, which can be in a music folder, at `rest`
// in it: in the path the user gave the folder, outside the sandbox, or a
// URI for a path that isn't UTF-8, as the playlist is
fn absolute_line(path: &Path, folder: Option<(&ExportFolder, &Path)>) -> String {
    if let Some((folder, rest)) = folder {
        if let (Some(mapped), Some(rest)) = (&folder.mapped, rest.to_str()) {
            return mapped_path(mapped, rest);
        }
        let written = folder.written.join(rest);
        if let Some(written) = written.to_str() {
            return written.to_string();
        }
        return gio::File::for_path(&written).uri().to_string();
    }
    path.to_str().map_or_else(
        || gio::File::for_path(path).uri().to_string(),
        str::to_string,
    )
}

// The path of a track in a music folder that the user gave the path
// `mapped`, which is written as it's given: with backslashes on Windows,
// as players there read them best
fn mapped_path(mapped: &str, rest: &str) -> String {
    let windows = mapped.as_bytes().get(1) == Some(&b':') || mapped.contains('\\');
    let separator = if windows { '\\' } else { '/' };
    let base = mapped.trim_end_matches(['/', '\\']);
    if rest.is_empty() {
        return base.to_string();
    }
    let rest: String = rest
        .chars()
        .map(|c| if c == '/' { separator } else { c })
        .collect();
    format!("{base}{separator}{rest}")
}

// The path of `to` relative to `dir`, both absolute and without symbolic
// links, climbing out of `dir` with ".." to the folder they share
fn relative_path(dir: &Path, to: &Path) -> PathBuf {
    let shared = dir
        .components()
        .zip(to.components())
        .take_while(|(a, b)| a == b)
        .count();
    let mut path: PathBuf = dir.components().skip(shared).map(|_| "..").collect();
    path.extend(to.components().skip(shared));
    path
}

pub(crate) fn save(name: &str, files: &[gio::File]) {
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
    glib::file_set_contents(path, format_m3u(files).as_bytes())
}

// A playlist of Dismas's own, with the absolute paths of the local files,
// and the URIs of the others. A track of a CUE sheet keeps its path under
// the sheet, which is how the library, the points to resume from and the
// play history know it, and which needs no sheet read to load the
// playlist
fn format_m3u(files: &[gio::File]) -> String {
    let mut text = String::from("#EXTM3U\n");
    for file in files {
        let path = file.path();
        let line: Cow<str> = match path.as_deref().and_then(Path::to_str) {
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
    text
}

// A playlist for other players, see `ExportPaths`. Other players can't
// read the path under its sheet that Dismas gives a track of a CUE sheet,
// so it's named as Strawberry names one, by the audio file the sheet
// splits, after a line with the track's number. Most players play the
// whole file; Strawberry and Dismas find the track
fn format_export(files: &[gio::File], paths: &mut ExportPaths) -> (String, ExportReport) {
    let mut text = String::from("#EXTM3U\n");
    let mut report = ExportReport::default();
    let mut sheets = HashMap::new();
    for file in files {
        let cue_track = file.path().and_then(|path| {
            let (sheet, number) = cue_sheet::missing_location_parts(&path)?;
            Some((cue_audio_file(sheet, number, &mut sheets), number))
        });
        let path = match &cue_track {
            Some((audio, _)) => Some(audio.clone()),
            None => file.path(),
        };
        let (line, relative) = match path {
            Some(path) => match paths.line(&path) {
                Some(line) => line,
                None => continue,
            },
            // Not a local file, which other devices reach as this one does
            None => (file.uri().to_string(), true),
        };
        if let Some((_, number)) = cue_track {
            text.push_str(&format!("{CUE_TRACK_LINE}{number}\n"));
            report.n_cue += 1;
        }
        if !relative && !paths.absolute {
            report.n_absolute += 1;
        }
        text.push_str(&line);
        text.push('\n');
    }
    (text, report)
}

// The file that names a track of a CUE sheet in an export: the audio file
// of the track, which a ".cue" file names, or the audio file that has the
// sheet in its tags. A ".cue" file that isn't there, or that can't be
// read, names its own track. The sheets read are kept, as an album's
// tracks share one
fn cue_audio_file(
    sheet: &Path,
    number: u32,
    sheets: &mut HashMap<PathBuf, Option<cue_sheet::SplitSheet>>,
) -> PathBuf {
    if !cue_sheet::is_cue_path(sheet) {
        return sheet.to_path_buf();
    }
    sheets
        .entry(sheet.to_path_buf())
        .or_insert_with(|| {
            utils::guarded(
                sheet.display(),
                || cue_sheet::SplitSheet::read(sheet),
                || None,
            )
        })
        .as_ref()
        .and_then(|split| split.track(number))
        .map_or_else(|| sheet.to_path_buf(), |(_, audio)| audio.to_path_buf())
}

// The entries of an M3U playlist, which can be URIs, or paths; relative
// paths are relative to `base`, the directory of the playlist
fn parse_m3u(text: &str, base: &gio::File) -> Vec<gio::File> {
    // Only read for a playlist with Strawberry's lines, which Dismas
    // doesn't write in its own
    let mut sheets = cue_sheet::FolderSheets::default();
    m3u_entries(text)
        .map(|entry| with_cue_track(m3u_entry(entry.line, base), entry.cue_track, &mut sheets))
        .collect()
}

// The line Strawberry writes before a track of a CUE sheet, with its
// number, as it names the track by the audio file the sheet splits
const CUE_TRACK_LINE: &str = "#EXTSTRAWBERRYTRACK:";

// An entry of an M3U playlist, and the number of the track of a CUE sheet
// that a line before it gave
struct M3uEntry<'a> {
    line: &'a str,
    cue_track: Option<u32>,
}

// The entries of an M3U playlist, the lines that name tracks. A number
// Strawberry gave holds until the next entry, across the other lines that
// start with "#", such as #EXTINF, which Strawberry writes before it or
// after it. Old Macs ended lines with a carriage return alone
fn m3u_entries(text: &str) -> impl Iterator<Item = M3uEntry<'_>> {
    let mut cue_track = None;
    text.split(['\n', '\r'])
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        // GLib can't take a path with a NUL byte, as in a UTF-16 playlist
        // without a byte order mark, or a file with a block of zeros
        .filter(|line| !line.is_empty() && !line.contains('\0'))
        .filter_map(move |line| {
            if let Some(number) = line.strip_prefix(CUE_TRACK_LINE) {
                cue_track = number
                    .trim()
                    .parse()
                    .ok()
                    .filter(|number| (1..=cue_sheet::MAX_TRACK_NUMBER).contains(number));
                None
            } else if line.starts_with('#') {
                None
            } else {
                Some(M3uEntry {
                    line,
                    cue_track: cue_track.take(),
                })
            }
        })
}

// The track of a CUE sheet that an entry names with Strawberry's number,
// if it does, and the file is local and isn't already a track of a sheet
fn with_cue_track(
    file: gio::File,
    cue_track: Option<u32>,
    sheets: &mut cue_sheet::FolderSheets,
) -> gio::File {
    let Some(number) = cue_track else {
        return file;
    };
    match file.path() {
        Some(path) if cue_sheet::missing_location_parts(&path).is_none() => {
            gio::File::for_path(cue_sheet::track_of_file(&path, number, sheets))
        }
        _ => file,
    }
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
        let text = format_m3u(&files);
        assert_eq!(text, "#EXTM3U\n/music/Café/01 a.opus\n/music/b.flac\n");
        let parsed = parse_m3u(&text, &gio::File::for_path("/"));
        assert_eq!(paths(&parsed), paths(&files));
    }

    #[test]
    fn format_skips_line_breaks() {
        let files = [gio::File::for_path("/music/a\nb.opus")];
        assert_eq!(format_m3u(&files), "#EXTM3U\n");
        assert_eq!(
            format_export(&files, &mut export_paths(Path::new("/music"))).0,
            "#EXTM3U\n"
        );
    }

    #[test]
    fn format_relative_to_a_directory() {
        let files = [
            gio::File::for_path("/home/me/Music/Album/01 a.opus"),
            gio::File::for_path("/home/me/Other/b.flac"),
        ];
        let (text, report) = format_export(&files, &mut export_paths(Path::new("/home/me/Music")));
        assert_eq!(text, "#EXTM3U\nAlbum/01 a.opus\n/home/me/Other/b.flac\n");
        assert_eq!(report.n_absolute, 1);
    }

    #[test]
    fn format_relative_paths_that_look_like_comments() {
        let files = [
            gio::File::for_path("/music/#1 Hits/a.opus"),
            gio::File::for_path("/music/ b.opus"),
        ];
        let (text, _) = format_export(&files, &mut export_paths(Path::new("/music")));
        assert_eq!(text, "#EXTM3U\n./#1 Hits/a.opus\n./ b.opus\n");
        let parsed = parse_m3u(&text, &gio::File::for_path("/music"));
        assert_eq!(paths(&parsed), paths(&files));
    }

    // The tracks of CUE sheets are under their sheet, which is a file
    #[test]
    fn private_playlists_keep_the_paths_of_cue_tracks() {
        let files = [
            gio::File::for_path("/home/me/Music/Album/Album.cue/track03"),
            gio::File::for_path("/home/me/Music/Live.flac/track12"),
        ];
        let text = format_m3u(&files);
        assert_eq!(
            text,
            "#EXTM3U\n/home/me/Music/Album/Album.cue/track03\n/home/me/Music/Live.flac/track12\n"
        );
        let parsed = parse_m3u(&text, &gio::File::for_path("/"));
        assert_eq!(paths(&parsed), paths(&files));
    }

    #[test]
    fn export_writes_cue_tracks_as_strawberry_does() {
        let dir = folder_with_a_sheet();
        let files = [gio::File::for_path(dir.join("Album.cue/track02"))];
        let (text, report) = format_export(&files, &mut export_paths(&dir));
        assert_eq!(text, "#EXTM3U\n#EXTSTRAWBERRYTRACK:2\nAlbum.flac\n");
        assert_eq!(
            report,
            ExportReport {
                n_absolute: 0,
                n_cue: 1
            }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_writes_the_sheet_of_a_missing_cue_track() {
        let files = [gio::File::for_path("/gone/Album.cue/track04")];
        let (text, report) = format_export(&files, &mut export_paths(Path::new("/gone")));
        assert_eq!(text, "#EXTM3U\n#EXTSTRAWBERRYTRACK:4\nAlbum.cue\n");
        assert_eq!(report.n_cue, 1);
    }

    #[test]
    fn export_of_a_sheet_in_tags_names_the_audio_file() {
        let files = [gio::File::for_path("/music/Live.flac/track12")];
        let (text, _) = format_export(&files, &mut export_paths(Path::new("/music")));
        assert_eq!(text, "#EXTM3U\n#EXTSTRAWBERRYTRACK:12\nLive.flac\n");
    }

    #[test]
    fn exported_cue_tracks_import_as_they_were() {
        let dir = folder_with_a_sheet();
        // A sheet whose tracks are in two files, after the one in one file
        for name in ["Side A.flac", "Side B.flac"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        std::fs::write(
            dir.join("Sides.cue"),
            "FILE \"Side A.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  \
             TRACK 02 AUDIO\n    INDEX 01 01:00:00\nFILE \"Side B.flac\" WAVE\n  \
             TRACK 03 AUDIO\n    INDEX 01 00:00:00\n  TRACK 04 AUDIO\n    INDEX 01 01:00:00\n",
        )
        .unwrap();
        let files: Vec<gio::File> = [
            "Album.cue/track01",
            "Sides.cue/track04",
            "Album.cue/track02",
            "Sides.cue/track02",
        ]
        .into_iter()
        .map(|track| gio::File::for_path(dir.join(track)))
        .collect();
        let playlist = dir.join("Mix.m3u8");
        let (text, _) = format_export(&files, &mut export_paths(&dir));
        assert!(
            text.contains("#EXTSTRAWBERRYTRACK:4\nSide B.flac\n"),
            "{text}"
        );
        std::fs::write(&playlist, text).unwrap();
        let imported = import_m3u(&playlist, &[]).unwrap();
        assert_eq!(paths(&imported), paths(&files));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_counts_tracks_of_cue_sheets() {
        let files = [
            gio::File::for_path("/music/a.flac"),
            gio::File::for_path("/music/Live.flac/track01"),
            gio::File::for_path("/elsewhere/Live.flac/track02"),
        ];
        let (_, report) = format_export(&files, &mut export_paths(Path::new("/music")));
        assert_eq!(
            report,
            ExportReport {
                n_absolute: 1,
                n_cue: 2
            }
        );
    }

    #[test]
    fn relative_paths_climb_out_of_the_playlist_folder() {
        let relative = |dir: &str, to: &str| relative_path(Path::new(dir), Path::new(to));
        assert_eq!(
            relative("/m/Playlists", "/m/A/1.flac"),
            Path::new("../A/1.flac")
        );
        assert_eq!(relative("/m", "/m/A/1.flac"), Path::new("A/1.flac"));
        assert_eq!(relative("/m/P/Q", "/m/A"), Path::new("../../A"));
        assert_eq!(relative("/a", "/b/c"), Path::new("../b/c"));
    }

    // Paths of an export to `dir`, with one music folder, read at `read`
    // and found outside the sandbox at `host`
    fn paths_with_folder(dir: &str, read: &str, host: &str, absolute: bool) -> ExportPaths {
        ExportPaths {
            dir: Some(PathBuf::from(dir)),
            folders: vec![ExportFolder {
                read: PathBuf::from(read),
                written: PathBuf::from(host),
                mapped: None,
                key: PathBuf::from(host),
            }],
            absolute,
            resolved: HashMap::new(),
        }
    }

    #[test]
    fn export_paths_are_relative_within_a_music_folder() {
        let mut paths = paths_with_folder(
            "/home/me/Music/Playlists",
            "/home/me/Music",
            "/home/me/Music",
            false,
        );
        assert_eq!(
            paths.line(Path::new("/home/me/Music/A/01.flac")),
            Some(("../A/01.flac".to_string(), true))
        );
        // A path that climbs can't start with "#"
        assert_eq!(
            paths.line(Path::new("/home/me/Music/Playlists/#1.flac")),
            Some(("./#1.flac".to_string(), true))
        );
    }

    #[test]
    fn export_paths_are_absolute_across_music_folders() {
        let mut paths = paths_with_folder(
            "/home/me/Music/Playlists",
            "/home/me/Music",
            "/home/me/Music",
            false,
        );
        paths.folders.push(ExportFolder {
            read: PathBuf::from("/media/hdd/Music"),
            written: PathBuf::from("/media/hdd/Music"),
            mapped: None,
            key: PathBuf::from("/media/hdd/Music"),
        });
        assert_eq!(
            paths.line(Path::new("/media/hdd/Music/A/01.flac")),
            Some(("/media/hdd/Music/A/01.flac".to_string(), false))
        );
        // Nor are the tracks outside any music folder relative
        assert_eq!(
            paths.line(Path::new("/home/me/Downloads/01.flac")),
            Some(("/home/me/Downloads/01.flac".to_string(), false))
        );
    }

    #[test]
    fn export_maps_portal_paths_to_host_paths() {
        let read = "/run/user/1000/doc/1a2b3c4d/Music";
        let track = Path::new("/run/user/1000/doc/1a2b3c4d/Music/A/01.flac");
        let mut paths = paths_with_folder("/srv/Music/Playlists", read, "/srv/Music", false);
        assert_eq!(paths.line(track), Some(("../A/01.flac".to_string(), true)));
        let mut paths = paths_with_folder("/srv/Music/Playlists", read, "/srv/Music", true);
        assert_eq!(
            paths.line(track),
            Some(("/srv/Music/A/01.flac".to_string(), false))
        );
    }

    #[test]
    fn the_absolute_setting_writes_every_path_absolute() {
        let files = [
            gio::File::for_path("/home/me/Music/A/01.flac"),
            gio::File::for_path("/home/me/Music/Playlists/02.flac"),
        ];
        let mut paths = paths_with_folder(
            "/home/me/Music/Playlists",
            "/home/me/Music",
            "/home/me/Music",
            true,
        );
        let (text, report) = format_export(&files, &mut paths);
        assert_eq!(
            text,
            "#EXTM3U\n/home/me/Music/A/01.flac\n/home/me/Music/Playlists/02.flac\n"
        );
        // As asked, so nothing to warn about
        assert_eq!(report.n_absolute, 0);
    }

    #[test]
    fn non_utf8_paths_are_uris_and_count_as_absolute() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = Path::new(OsStr::from_bytes(b"/music/caf\xe9.flac"));
        let files = [gio::File::for_path(path)];
        let (text, report) = format_export(&files, &mut export_paths(Path::new("/music")));
        assert_eq!(text, "#EXTM3U\nfile:///music/caf%E9.flac\n");
        assert_eq!(report.n_absolute, 1);
        let parsed = parse_m3u(&text, &gio::File::for_path("/music"));
        assert_eq!(parsed[0].path().as_deref(), Some(path));
    }

    // A temporary folder, its path without symbolic links, as the system's
    // temporary folder can be one, and an album in it
    fn folder_with_an_album() -> (PathBuf, PathBuf) {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let real = std::fs::canonicalize(&dir).unwrap();
        std::fs::create_dir_all(real.join("data/music/A")).unwrap();
        std::fs::create_dir_all(real.join("data/music/Playlists")).unwrap();
        std::fs::write(real.join("data/music/A/01.flac"), b"").unwrap();
        (dir, real)
    }

    #[test]
    fn an_export_folder_behind_a_symbolic_link_gets_relative_paths() {
        let (dir, real) = folder_with_an_album();
        std::os::unix::fs::symlink(real.join("data/music/Playlists"), real.join("Playlists"))
            .unwrap();
        let folders = [MusicFolder::new(real.join("data/music"))];
        let file = real.join("Playlists/Mix.m3u8");
        let mut paths = ExportPaths::new(export_dir(&file), &options(&folders));
        let files = [gio::File::for_path(real.join("data/music/A/01.flac"))];
        assert_eq!(
            format_export(&files, &mut paths).0,
            "#EXTM3U\n../A/01.flac\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_music_folder_that_is_a_symbolic_link_gets_relative_paths() {
        let (dir, real) = folder_with_an_album();
        // As ~/Music can lead to another drive
        std::os::unix::fs::symlink(real.join("data/music"), real.join("Music")).unwrap();
        let folders = [MusicFolder::new(real.join("Music"))];
        let file = real.join("Music/Playlists/Mix.m3u8");
        let mut paths = ExportPaths::new(export_dir(&file), &options(&folders));
        let files = [gio::File::for_path(real.join("Music/A/01.flac"))];
        assert_eq!(
            format_export(&files, &mut paths).0,
            "#EXTM3U\n../A/01.flac\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // The paths of an export to `dir`, outside the music folders
    fn export_paths(dir: &Path) -> ExportPaths {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        ExportPaths::new(Some(dir), &options(&[]))
    }

    fn options(music_folders: &[MusicFolder]) -> ExportOptions {
        ExportOptions {
            music_folders: music_folders.to_vec(),
            mappings: HashMap::new(),
            absolute: false,
        }
    }

    #[test]
    fn mapped_music_folders_start_the_absolute_paths() {
        let mut paths = paths_with_folder(
            "/home/me/Music/Playlists",
            "/home/me/Music",
            "/home/me/Music",
            true,
        );
        paths.folders[0].mapped = Some("/music/".to_string());
        assert_eq!(
            paths.line(Path::new("/home/me/Music/A/01.flac")),
            Some(("/music/A/01.flac".to_string(), false))
        );
        // The folder's own tracks, and those outside it
        assert_eq!(
            paths.line(Path::new("/home/me/Music/01.flac")),
            Some(("/music/01.flac".to_string(), false))
        );
        assert_eq!(
            paths.line(Path::new("/home/me/Other/01.flac")),
            Some(("/home/me/Other/01.flac".to_string(), false))
        );
    }

    #[test]
    fn tracks_with_relative_paths_are_not_mapped() {
        let mut paths = paths_with_folder(
            "/home/me/Music/Playlists",
            "/home/me/Music",
            "/home/me/Music",
            false,
        );
        paths.folders[0].mapped = Some("/storage/emulated/0/Music".to_string());
        assert_eq!(
            paths.line(Path::new("/home/me/Music/A/01.flac")),
            Some(("../A/01.flac".to_string(), true))
        );
    }

    #[test]
    fn windows_mappings_use_backslashes() {
        let mut paths = paths_with_folder("/elsewhere", "/home/me/Music", "/home/me/Music", false);
        for (mapped, line) in [
            ("D:\\Music\\", "D:\\Music\\A\\01.flac"),
            ("D:", "D:\\A\\01.flac"),
            ("\\\\nas\\music", "\\\\nas\\music\\A\\01.flac"),
        ] {
            paths.folders[0].mapped = Some(mapped.to_string());
            assert_eq!(
                paths.line(Path::new("/home/me/Music/A/01.flac")),
                Some((line.to_string(), false)),
                "{mapped}"
            );
        }
    }

    // A folder with "Album.cue", which splits "Album.flac" into two tracks
    fn folder_with_a_sheet() -> PathBuf {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        std::fs::write(dir.join("Album.flac"), b"").unwrap();
        std::fs::write(
            dir.join("Album.cue"),
            "FILE \"Album.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  \
             TRACK 02 AUDIO\n    INDEX 01 01:00:00\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn parse_reads_strawberry_cue_tracks() {
        let dir = folder_with_a_sheet();
        let base = gio::File::for_path(&dir);
        // Strawberry writes #EXTINF first, and reads either order
        let text = "#EXTM3U\n#EXTINF:60,A - Two\n#EXTSTRAWBERRYTRACK:2\nAlbum.flac\n\
                    #EXTSTRAWBERRYTRACK:1\n#EXTINF:60,A - One\nAlbum.flac\n";
        let files = parse_m3u(text, &base);
        assert_eq!(
            paths(&files),
            [dir.join("Album.cue/track02"), dir.join("Album.cue/track01")]
                .map(|path| path.to_str().unwrap().to_string())
        );

        // A sheet names its own track
        let files = parse_m3u("#EXTSTRAWBERRYTRACK:3\n/music/Live.cue\n", &base);
        assert_eq!(paths(&files), ["/music/Live.cue/track03"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cue_track_line_applies_to_the_next_entry_only() {
        let dir = folder_with_a_sheet();
        let text = "#EXTSTRAWBERRYTRACK:2\nAlbum.flac\nAlbum.flac\n";
        let files = parse_m3u(text, &gio::File::for_path(&dir));
        assert_eq!(
            paths(&files),
            [dir.join("Album.cue/track02"), dir.join("Album.flac")]
                .map(|path| path.to_str().unwrap().to_string())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cue_track_lines_with_bad_numbers_are_ignored() {
        for number in ["0", "-1", "1000", "x", "", "2 3"] {
            let text = format!("#EXTSTRAWBERRYTRACK:{number}\n/music/Album.flac\n");
            let files = parse_m3u(&text, &gio::File::for_path("/music"));
            assert_eq!(paths(&files), ["/music/Album.flac"], "{number:?}");
        }
        // The last of two holds
        let text = "#EXTSTRAWBERRYTRACK:1\n#EXTSTRAWBERRYTRACK:4\n/music/Live.cue\n";
        let files = parse_m3u(text, &gio::File::for_path("/music"));
        assert_eq!(paths(&files), ["/music/Live.cue/track04"]);
        // An entry that's already a track of a sheet keeps its number
        let text = "#EXTSTRAWBERRYTRACK:1\n/music/Live.cue/track02\n";
        let files = parse_m3u(text, &gio::File::for_path("/music"));
        assert_eq!(paths(&files), ["/music/Live.cue/track02"]);
    }

    #[test]
    fn strawberry_tracks_of_audio_files_go_to_the_sheet_that_splits_them() {
        let dir = folder_with_a_sheet();
        // Imported from elsewhere, where the album was in another folder
        let playlist = dir.join("list.m3u8");
        std::fs::write(
            &playlist,
            "#EXTSTRAWBERRYTRACK:2\nC:\\Music\\Old\\Album.flac\n",
        )
        .unwrap();
        let moved = dir.join("Music");
        std::fs::create_dir_all(moved.join("Old")).unwrap();
        std::fs::rename(dir.join("Album.flac"), moved.join("Old/Album.flac")).unwrap();
        std::fs::rename(dir.join("Album.cue"), moved.join("Old/Album.cue")).unwrap();
        let files = import_m3u(&playlist, std::slice::from_ref(&moved)).unwrap();
        assert_eq!(
            paths(&files),
            [moved.join("Old/Album.cue/track02").to_str().unwrap()]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // The fallback a sheet in the tags needs: an audio file that no ".cue"
    // file splits names the track of a sheet in its tags, and a file
    // without one gives a track that can't be read, rather than the whole
    // file once for each of its tracks
    #[test]
    fn a_cue_track_line_for_a_file_without_a_sheet_names_a_sheet_in_its_tags() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        std::fs::write(dir.join("Live.flac"), b"").unwrap();
        let files = parse_m3u(
            "#EXTSTRAWBERRYTRACK:2\nLive.flac\n",
            &gio::File::for_path(&dir),
        );
        assert_eq!(
            paths(&files),
            [dir.join("Live.flac/track02").to_str().unwrap()]
        );
        assert!(cue_sheet::location_exists(&dir.join("Live.flac/track02")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_limit_on_entries_does_not_count_cue_track_lines() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let playlist = dir.join("list.m3u");
        std::fs::write(&playlist, "#EXTSTRAWBERRYTRACK:1\na.cue\n").unwrap();
        let files = import_m3u_within(&playlist, &[], 1024, 1).unwrap();
        assert_eq!(paths(&files), [dir.join("a.cue/track01").to_str().unwrap()]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // Dismas can only play a part of a file as the track of a CUE sheet, so
    // VLC's times are left out, and the whole file plays
    #[test]
    fn vlc_options_are_ignored() {
        let text = "#EXTVLCOPT:start-time=10\n#EXTVLCOPT:stop-time=20\n/music/a.flac\n";
        let files = parse_m3u(text, &gio::File::for_path("/music"));
        assert_eq!(paths(&files), ["/music/a.flac"]);
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
    fn imported_names_match_in_either_unicode_form() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let music = dir.join("Music");
        std::fs::create_dir_all(music.join("Ensemble")).unwrap();
        let composed = "M\u{fc}ller.flac";
        let decomposed = "Mu\u{308}ller.flac";
        let folders = [music.clone()];
        for (on_disk, in_playlist) in [(decomposed, composed), (composed, decomposed)] {
            let file = music.join("Ensemble").join(on_disk);
            std::fs::write(&file, b"").unwrap();
            let line = format!("/Users/me/Music/Ensemble/{in_playlist}");
            let found = Finder::default().find(&line, None, &folders);
            assert_eq!(found.as_deref(), Some(file.as_path()), "{in_playlist:?}");
            std::fs::remove_file(&file).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_that_differ_only_in_unicode_form_are_ambiguous() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let album = dir.join("Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("M\u{fc}ller.flac"), b"").unwrap();
        std::fs::write(album.join("Mu\u{308}ller.flac"), b"").unwrap();
        let found = Finder::default().find("C:\\Album\\M\u{dc}LLER.flac", Some(&dir), &[]);
        assert_eq!(found, None);
        // As it's written, it's one of them
        let found = Finder::default().find("C:\\Album\\M\u{fc}ller.flac", Some(&dir), &[]);
        assert_eq!(found, Some(album.join("M\u{fc}ller.flac")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cue_track_path_needs_a_folder_and_its_sheet() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let music = dir.join("Music");
        std::fs::create_dir_all(music.join("Album")).unwrap();
        std::fs::write(music.join("Live.cue"), b"").unwrap();
        std::fs::write(music.join("Album/Album.cue"), b"").unwrap();
        let folders = [music.clone()];
        let mut finder = Finder::default();
        // A sheet alone is a file name, which could be anywhere
        assert_eq!(finder.find("/old/Live.cue/track02", None, &folders), None);
        assert_eq!(
            finder.find("/old/Album/Album.cue/track02", None, &folders),
            Some(music.join("Album/Album.cue/track02"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tracks_of_cue_sheets_are_found_where_they_moved() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let music = dir.join("Music");
        std::fs::create_dir_all(music.join("Artist/Album")).unwrap();
        std::fs::write(music.join("Artist/Album/Album.cue"), b"").unwrap();
        std::fs::write(music.join("Artist/Album/Live.flac"), b"").unwrap();
        let folders = [music.clone()];
        let mut finder = Finder::default();
        assert_eq!(
            finder.find(
                "D:\\music\\artist\\ALBUM\\album.CUE\\track02",
                None,
                &folders
            ),
            Some(music.join("Artist/Album/Album.cue/track02"))
        );
        // A sheet in the tags of an audio file
        assert_eq!(
            finder.find(
                "/home/me/Musik/Artist/Album/Live.flac/track11",
                None,
                &folders
            ),
            Some(music.join("Artist/Album/Live.flac/track11"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_playlist_folder_only_the_music_folders_are_tried() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let music = dir.join("Music");
        std::fs::create_dir_all(music.join("Album")).unwrap();
        std::fs::write(dir.join("01.flac"), b"").unwrap();
        std::fs::write(music.join("Album/02.flac"), b"").unwrap();
        let folders = [music.clone()];
        let mut finder = Finder::default();
        assert_eq!(
            finder.find("/old/01.flac", Some(&dir), &folders),
            Some(dir.join("01.flac"))
        );
        assert_eq!(finder.find("/old/01.flac", None, &folders), None);
        assert_eq!(finder.find("01.flac", None, &folders), None);
        assert_eq!(
            finder.find("/old/Album/02.flac", None, &folders),
            Some(music.join("Album/02.flac"))
        );
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
