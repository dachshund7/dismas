// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// Marks a file of a cache as used, so that `remove_old_files()` keeps it
pub(crate) fn touch(path: &std::path::Path) {
    let result = std::fs::File::options()
        .append(true)
        .open(path)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()));
    if let Err(e) = result {
        debug!("Unable to mark {path:?} as used: {e}");
    }
}

// Removes the files of a cache folder that weren't changed or used for
// `age`. The caches of waveforms and covers would otherwise only grow, with
// a file for every track and album ever played; one that's needed again is
// made again
pub(crate) fn remove_old_files(dir: &std::path::Path, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|since| since > age);
        if metadata.is_file()
            && old
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            warn!("Unable to remove {:?}: {e}", entry.path());
        }
    }
}

// Opens a file that anyone could have put there, such as a cover next to
// the tracks or a track of a playlist, if it's a regular file. Opening a
// named pipe would wait for something to write to it, which stopped the
// queue loader, or a scan, for good. Checking the path first isn't enough,
// as a symbolic link can be swapped for a pipe in between, so the file is
// opened without waiting, and the file opened is checked
pub(crate) fn open_regular_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    // So that a device is never opened, as far as the path tells
    if !std::fs::metadata(path)?.is_file() {
        return Err(not_a_regular_file());
    }
    open_if_regular(path)
}

pub(super) fn open_if_regular(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(not_a_regular_file());
    }
    // Reading a regular file never waits, but it's read as usual, so that
    // nothing reading it is ever told to try again
    let fd = file.as_raw_fd();
    // SAFETY: the descriptor is open for both calls, which only change its
    // flags
    let cleared = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags != -1 && libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) != -1
    };
    if !cleared {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

fn not_a_regular_file() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file")
}

// Whether a file is an audio track, guessing from its name only, which is
// much faster than looking at its contents when scanning a whole library
pub(crate) fn is_audio_path(path: &std::path::Path) -> bool {
    let (mime_type, _) = gio::content_type_guess(Some(path), None);
    is_audio_type(&mime_type)
}

// Playlists have audio types too. WebM files have a video type, but are
// nearly always downloaded music in the music folders, with a video or
// not, and only their sound is played
pub(crate) fn is_audio_type(mime_type: &str) -> bool {
    const PLAYLISTS: [&str; 4] = [
        "audio/x-mpegurl",
        "audio/mpegurl",
        "audio/x-scpls",
        "audio/x-ms-asx",
    ];
    (mime_type.starts_with("audio/") && !PLAYLISTS.contains(&mime_type))
        || mime_type == "video/webm"
}

// What a walk through a folder found
#[derive(Default)]
pub(crate) struct FolderTree {
    // In the order a file manager shows them
    pub(crate) files: Vec<gio::File>,
    // The folders read, the first one included, in no order
    pub(crate) folders: Vec<gio::File>,
    // The symbolic links to folders that were followed
    pub(crate) links: Vec<gio::File>,
    // Whether a symbolic link leads nowhere, as one to a drive that isn't
    // mounted does
    pub(crate) broken_links: bool,
    // The folders that couldn't be read whole, e.g. on a network share that
    // stopped answering: the files in them that weren't found can still be
    // there. Not those that are gone, or aren't folders anymore
    pub(crate) unread: Vec<std::path::PathBuf>,
}

// Following symbolic links, the type is that of what they lead to
const WALK_ATTRIBUTES: &str = "standard::name,standard::type,standard::is-symlink,\
                               unix::device,unix::inode";

struct FolderWalk<'a> {
    recursive: bool,
    keep: &'a dyn Fn(&std::path::Path) -> bool,
    // The device and inode of the folders and files read, which are the
    // same however they're reached
    read: std::collections::HashSet<(u32, u64)>,
    // The links found, which are followed once the folders are read
    links: Vec<(gio::File, gio::FileInfo)>,
    tree: FolderTree,
}

impl FolderWalk<'_> {
    fn read_folder(&mut self, folder: &gio::File) {
        self.tree.folders.push(folder.clone());
        let mut enumerator = match folder.enumerate_children(
            WALK_ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            None::<&gio::Cancellable>,
        ) {
            Ok(enumerator) => enumerator,
            Err(e) => {
                warn!("Unable to enumerate {}: {e}", folder.uri());
                self.unread(folder, &e);
                return;
            }
        };
        loop {
            let info = match enumerator.next() {
                Some(Ok(info)) => info,
                Some(Err(e)) => {
                    warn!("Unable to read the whole of {}: {e}", folder.uri());
                    self.unread(folder, &e);
                    break;
                }
                None => break,
            };
            let child = enumerator.child(&info);
            if child.path().is_some_and(|path| !(self.keep)(&path)) {
                continue;
            }
            if info.is_symlink() {
                self.links.push((child, info));
            } else {
                self.add(child, &info, false);
            }
        }
    }

    fn unread(&mut self, folder: &gio::File, error: &glib::Error) {
        if may_have_files(error)
            && let Some(path) = folder.path()
        {
            self.tree.unread.push(path);
        }
    }

    // A folder is read once, so a link can't make a loop; a file a link
    // leads to is only kept if it wasn't read already, but the files
    // themselves are all kept, as hard links to one file can be meant
    fn add(&mut self, file: gio::File, info: &gio::FileInfo, linked: bool) {
        match info.file_type() {
            gio::FileType::Directory if self.recursive => {
                if self.first_read(info) {
                    if linked {
                        self.tree.links.push(file.clone());
                    }
                    self.read_folder(&file);
                }
            }
            gio::FileType::Regular => {
                if self.first_read(info) || !linked {
                    self.tree.files.push(file);
                }
            }
            // A link that leads nowhere keeps its own type
            gio::FileType::SymbolicLink => self.tree.broken_links = true,
            _ => (),
        }
    }

    // Files that aren't local have no inode, and can't be told apart
    fn first_read(&mut self, info: &gio::FileInfo) -> bool {
        let inode = info.attribute_uint64(gio::FILE_ATTRIBUTE_UNIX_INODE);
        inode == 0
            || self.read.insert((
                info.attribute_uint32(gio::FILE_ATTRIBUTE_UNIX_DEVICE),
                inode,
            ))
    }
}

// Whether a folder that gave `error` while it was read can still have
// files: unless it isn't there anymore, or isn't a folder
pub(super) fn may_have_files(error: &glib::Error) -> bool {
    !error.matches(gio::IOErrorEnum::NotFound) && !error.matches(gio::IOErrorEnum::NotDirectory)
}

// The files and folders in a folder, without the ones whose paths `keep`
// turns down. Symbolic links are followed once the folders without them
// are read, so that one leading back into the folder adds nothing: each
// round follows the links found the round before, in the order of their
// paths, so that of two links to one folder, the same one is read at
// each scan
pub(crate) fn read_folder_tree(
    folder: &gio::File,
    recursive: bool,
    keep: &dyn Fn(&std::path::Path) -> bool,
) -> FolderTree {
    let mut walk = FolderWalk {
        recursive,
        keep,
        read: std::collections::HashSet::new(),
        links: Vec::new(),
        tree: FolderTree::default(),
    };
    if let Ok(info) = folder.query_info(
        WALK_ATTRIBUTES,
        gio::FileQueryInfoFlags::NONE,
        None::<&gio::Cancellable>,
    ) {
        walk.first_read(&info);
    }
    walk.read_folder(folder);
    while !walk.links.is_empty() {
        let mut links = std::mem::take(&mut walk.links);
        links.sort_by_cached_key(|(file, _)| file.path());
        for (file, info) in links {
            walk.add(file, &info, true);
        }
    }
    // gio::FileEnumerator has no guaranteed order. The files are sorted
    // once, with the keys made once for each file: a library of thousands
    // of files was sorted again at each level of its folders, with the keys
    // made again at each comparison
    walk.tree
        .files
        .sort_by_cached_key(|file| file_sort_key(Some(folder), file));
    walk.tree
}

// What files are sorted by, in the order a file manager shows them: the
// folder they're in, relative to the base folder, then their name
fn file_sort_key(
    base: Option<&gio::File>,
    file: &gio::File,
) -> (
    (bool, glib::FilenameCollationKey),
    (bool, glib::FilenameCollationKey),
) {
    (
        file_name_key(&folder_sort_name(base, file)),
        file_name_key(&file_sort_name(file)),
    )
}

// The folder of the file, relative to the base folder if it's inside it,
// or else by its own name
fn folder_sort_name(base: Option<&gio::File>, file: &gio::File) -> String {
    let Some(parent) = file.parent() else {
        return String::new();
    };
    base.and_then(|base| base.relative_path(&parent))
        .or_else(|| parent.basename())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// Files that aren't paths, like some GVFS locations, have no basename
fn file_sort_name(file: &gio::File) -> String {
    file.basename().map_or_else(
        || file.uri().to_string(),
        |path| path.to_string_lossy().into_owned(),
    )
}

pub(crate) fn cmp_file_names(filename_a: &str, filename_b: &str) -> Ordering {
    file_name_key(filename_a).cmp(&file_name_key(filename_b))
}

// The key a name sorts by: the names starting with "." or "#" go last, and
// the others in the order of file names in the locale, with numbers in
// their order, e.g. "Disc 2" before "Disc 10"
pub(crate) fn file_name_key(name: &str) -> (bool, glib::FilenameCollationKey) {
    (
        name.starts_with(['.', '#']),
        glib::FilenameCollationKey::from(name),
    )
}

#[cfg(test)]
pub(crate) fn load_files_from_folder(folder: &gio::File, recursive: bool) -> Vec<gio::File> {
    load_files_from_folder_filtered(folder, recursive, &|_| true)
}

// The files of a folder, without the files and folders whose paths `keep`
// turns down
pub(crate) fn load_files_from_folder_filtered(
    folder: &gio::File,
    recursive: bool,
    keep: &dyn Fn(&std::path::Path) -> bool,
) -> Vec<gio::File> {
    use std::time::Instant;

    let now = Instant::now();
    let res = read_folder_tree(folder, recursive, keep).files;
    debug!(
        "Folder enumeration: {} us (recursive: {}), total files: {}",
        now.elapsed().as_micros(),
        recursive,
        res.len()
    );

    res
}
