// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// A folder whose albums are in the grid: the path that's read, and the
// path outside the sandbox of a folder that came through the document
// portal, which names it and tells it apart even when it's gone, e.g.
// when its access was taken back. The folders in it that are left out are
// relative to it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MusicFolder {
    pub(crate) path: PathBuf,
    pub(crate) host: Option<PathBuf>,
    pub(crate) excluded: Vec<PathBuf>,
}

impl MusicFolder {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            host: None,
            excluded: Vec::new(),
        }
    }

    // The path to show
    pub(crate) fn shown_path(&self) -> &Path {
        self.host.as_deref().unwrap_or(&self.path)
    }

    // The paths of the folders left out, as they're read
    pub(crate) fn excluded_paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.excluded
            .iter()
            .map(|relative| self.path.join(relative))
    }
}

// The XDG Music folder; without a user-dirs.dirs file, GLib doesn't know
// about one, so we look for the usual "Music" folder in the home directory
pub(crate) fn default_music_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Music)
        .filter(|dir| *dir != glib::home_dir())
        .unwrap_or_else(|| glib::home_dir().join("Music"))
}

// The music folders in the settings, where an empty list means the XDG
// Music folder, with the folders they leave out. They're kept as URIs, as
// the strings of the settings must be UTF-8; the ones without a local path
// are left out
pub(crate) fn music_folders(settings: &gio::Settings) -> Vec<MusicFolder> {
    folders_from_entries(
        settings.get::<Vec<(String, String)>>("music-folders"),
        settings.get::<Vec<String>>("excluded-folders"),
    )
}

// The paths that exported playlists give the music folders, as a server
// that mounts them, another device or Windows names them, by the path each
// folder is read at. They're kept by its URI, and as they're given, as
// they're paths on another system. One that can't be a path is left out,
// as the settings can be changed outside Preferences
pub(crate) fn export_mappings(settings: &gio::Settings) -> HashMap<PathBuf, String> {
    settings
        .get::<HashMap<String, String>>("export-path-mappings")
        .into_iter()
        .filter(|(_, mapped)| is_mapped_path(mapped))
        .filter_map(|(uri, mapped)| Some((gio::File::for_uri(&uri).path()?, mapped)))
        .collect()
}

// Whether a path the user gives a music folder for exported playlists can
// be one: absolute, on a Unix system, or on Windows with a drive letter,
// as in "D:\Music", or on a network share, as in "\\server\music"
pub(crate) fn is_mapped_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    !path.contains(['\n', '\r']) && (path.starts_with('/') || drive || path.starts_with("\\\\"))
}

// Gives a music folder a path in exported playlists, or takes it away
// with an empty one, keeping those of the other music folders that are
// still there
pub(crate) fn set_export_mapping(settings: &gio::Settings, folder: &Path, mapped: &str) {
    let folders = music_folders(settings);
    let mut mappings = export_mappings(settings);
    mappings.retain(|path, _| folders.iter().any(|folder| folder.path == *path));
    if mapped.is_empty() {
        mappings.remove(folder);
    } else {
        mappings.insert(folder.to_path_buf(), mapped.to_string());
    }
    let entries: HashMap<String, String> = mappings
        .into_iter()
        .map(|(path, mapped)| (gio::File::for_path(path).uri().to_string(), mapped))
        .collect();
    if let Err(e) = settings.set("export-path-mappings", entries.to_variant()) {
        warn!("Unable to store the paths of the music folders in exported playlists: {e}");
    }
}

// The paths of the music folders, to look for tracks in
pub(crate) fn music_folder_paths() -> Vec<PathBuf> {
    music_folders(&utils::settings_manager())
        .into_iter()
        .map(|folder| folder.path)
        .collect()
}

pub(super) fn folders_from_entries(
    entries: Vec<(String, String)>,
    excluded: Vec<String>,
) -> Vec<MusicFolder> {
    let mut folders: Vec<MusicFolder> = if entries.is_empty() {
        vec![MusicFolder::new(default_music_dir())]
    } else {
        entries.into_iter().filter_map(folder_from_entry).collect()
    };

    // A folder that's left out belongs to the music folder it's in; one
    // that isn't in any is forgotten when the folders are stored again
    for path in excluded
        .iter()
        .filter_map(|uri| gio::File::for_uri(uri).path())
    {
        let found = folders.iter_mut().find_map(|folder| {
            let relative = path
                .strip_prefix(&folder.path)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())?
                .to_path_buf();
            Some((folder, relative))
        });
        if let Some((folder, relative)) = found
            && !folder.excluded.contains(&relative)
        {
            folder.excluded.push(relative);
        }
    }
    folders
}

// A folder as the settings keep it: the URI of the path that's read, and
// the URI of its path outside the sandbox, or an empty one. The folders
// it leaves out are kept apart
pub(crate) fn folder_from_entry((uri, host): (String, String)) -> Option<MusicFolder> {
    let path = gio::File::for_uri(&uri).path()?;
    let host = Some(host)
        .filter(|host| !host.is_empty())
        .and_then(|host| gio::File::for_uri(&host).path());
    Some(MusicFolder {
        path,
        host,
        excluded: Vec::new(),
    })
}

pub(crate) fn folder_entry(folder: &MusicFolder) -> (String, String) {
    let host = folder.host.as_deref().map(path_uri).unwrap_or_default();
    (path_uri(&folder.path), host)
}

fn path_uri(path: &Path) -> String {
    gio::File::for_path(path).uri().to_string()
}

pub(crate) fn store_music_folders(settings: &gio::Settings, folders: &[MusicFolder]) {
    let entries: Vec<(String, String)> = folders.iter().map(folder_entry).collect();
    let excluded: Vec<String> = folders
        .iter()
        .flat_map(MusicFolder::excluded_paths)
        .map(|path| path_uri(&path))
        .collect();
    if let Err(e) = settings
        .set("music-folders", entries.to_variant())
        .and_then(|()| settings.set("excluded-folders", excluded.to_variant()))
    {
        warn!("Unable to store the music folders: {e}");
    }
}

// A folder picked in the file chooser, as it's read. The portal gives a
// folder of the Music folder a path of its own, as it grants write access,
// which the sandbox doesn't have; the Music folder's own path is read
// instead, spelled like the Music folder, so that its tracks have the
// same paths either way. This blocks, as the portal's files can be slow
pub(crate) fn resolve_folder(path: &Path) -> MusicFolder {
    let Some(host) = host_path(path) else {
        return MusicFolder::new(path.to_path_buf());
    };
    // The host path has its symbolic links resolved, as the Music folder
    // might have, e.g. to another drive
    let music = default_music_dir();
    if let Ok(real_music) = std::fs::canonicalize(&music)
        && let Ok(relative) = host.strip_prefix(&real_music)
        && music.join(relative).is_dir()
    {
        return MusicFolder::new(music.join(relative));
    }
    MusicFolder {
        host: Some(host),
        ..MusicFolder::new(path.to_path_buf())
    }
}

// The path outside the sandbox of a folder or a file of the document
// portal
pub(crate) fn host_path(path: &Path) -> Option<PathBuf> {
    if !path.starts_with(glib::user_runtime_dir().join("doc")) {
        return None;
    }
    let info = gio::File::for_path(path)
        .query_info(
            utils::ATTRIBUTE_HOST_PATH,
            gio::FileQueryInfoFlags::NONE,
            None::<&gio::Cancellable>,
        )
        .ok()?;
    info.attribute_as_string(utils::ATTRIBUTE_HOST_PATH)
        .map(|host| PathBuf::from(host.as_str()))
}

// What tells folders apart, and whether one is in another: the path
// outside the sandbox, or the path without symbolic links, as two paths
// can lead to the same folder. This blocks
pub(crate) fn folder_key(folder: &MusicFolder) -> PathBuf {
    folder.host.clone().unwrap_or_else(|| {
        std::fs::canonicalize(&folder.path).unwrap_or_else(|_| folder.path.clone())
    })
}

// The folders to read, and those that aren't there, whose tracks are
// kept, as reading them again when they're back takes a while, but hidden,
// as they can't play. The folders left out of them aren't read, and their
// tracks are hidden until the scan forgets them
#[derive(Debug, Default)]
pub(crate) struct Roots {
    pub(crate) available: Vec<PathBuf>,
    pub(crate) unavailable: Vec<PathBuf>,
    pub(crate) excluded: Vec<PathBuf>,
}

// The music folders, without those in another one; this blocks
pub(crate) fn roots(folders: &[MusicFolder]) -> Roots {
    let keyed = folders
        .iter()
        .map(|folder| (folder_key(folder), folder))
        .collect();
    let mut roots = Roots::default();
    for folder in outermost(keyed) {
        roots.excluded.extend(folder.excluded_paths());
        if is_available(&folder.path) {
            roots.available.push(folder.path.clone());
        } else {
            roots.unavailable.push(folder.path.clone());
        }
    }
    roots
}

// The path of a folder relative to a music folder it's in, to leave it
// out; this blocks
pub(crate) fn relative_to(folder: &MusicFolder, music_folder: &MusicFolder) -> Option<PathBuf> {
    folder_key(folder)
        .strip_prefix(folder_key(music_folder))
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

// The values whose keys are neither in another's, nor the same as an
// earlier one's
pub(super) fn outermost<T>(keyed: Vec<(PathBuf, T)>) -> Vec<T> {
    let kept: Vec<bool> = keyed
        .iter()
        .enumerate()
        .map(|(i, (key, _))| {
            !keyed
                .iter()
                .enumerate()
                .any(|(j, (other, _))| key.starts_with(other) && (key != other || j < i))
        })
        .collect();
    keyed
        .into_iter()
        .zip(kept)
        .filter_map(|((_, value), kept)| kept.then_some(value))
        .collect()
}

// Whether a folder can be read: an empty one counts as gone, as it's often
// where a drive is mounted, while it isn't
pub(crate) fn is_available(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

// Hidden files and folders aren't part of the library, like the trash of
// a drive, which keeps deleted files under their own names, and neither
// are the recycle bins of Windows and of network drives, nor their
// snapshots, which keep copies of the files, nor Synology's thumbnails
pub(crate) fn is_library_entry(path: &Path) -> bool {
    const LEFT_OUT: [&[u8]; 6] = [
        b"$recycle.bin",
        b"recycler",
        b"#recycle",
        b"#snapshot",
        b"@recycle",
        b"@eadir",
    ];
    let Some(name) = path.file_name() else {
        return true;
    };
    let name = name.as_encoded_bytes();
    !name.starts_with(b".")
        && !LEFT_OUT
            .iter()
            .any(|left_out| name.eq_ignore_ascii_case(left_out))
}
