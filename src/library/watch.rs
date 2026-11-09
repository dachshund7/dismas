// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use gtk::{gio, glib, prelude::*};
use log::{debug, warn};

use super::{WatchList, is_library_entry};

// How long the folders stay without changes before they're read again, as
// copying an album changes its files many times over
const SETTLE_TIME: Duration = Duration::from_secs(2);

// Watches the folders of the library for files that come, go or change,
// and for drives with music folders that are mounted or unmounted, and
// calls `changed` once nothing changed for `SETTLE_TIME`, for a scan to
// read them again. Each folder has a monitor of its own, as GLib's don't
// look into the folders in a folder
pub(crate) struct FolderWatch {
    monitors: RefCell<HashMap<PathBuf, gio::FileMonitor>>,
    // The music folders, see `WatchList`
    available: RefCell<Vec<PathBuf>>,
    unavailable: Cell<bool>,
    pending: RefCell<Option<glib::SourceId>>,
    changed: Box<dyn Fn()>,
    volume_monitor: gio::VolumeMonitor,
    mount_handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl FolderWatch {
    pub(crate) fn new(changed: impl Fn() + 'static) -> Rc<Self> {
        let watch = Rc::new(Self {
            monitors: RefCell::default(),
            available: RefCell::default(),
            unavailable: Cell::new(false),
            pending: RefCell::default(),
            changed: Box::new(changed),
            volume_monitor: gio::VolumeMonitor::get(),
            mount_handlers: RefCell::default(),
        });

        // A music folder on a drive is there once it's mounted, and gone
        // once it's unmounted
        let weak = Rc::downgrade(&watch);
        let added = watch.volume_monitor.connect_mount_added(move |_, mount| {
            if let Some(watch) = weak.upgrade() {
                watch.mount_changed(mount);
            }
        });
        let weak = Rc::downgrade(&watch);
        let removed = watch.volume_monitor.connect_mount_removed(move |_, mount| {
            if let Some(watch) = weak.upgrade() {
                watch.mount_changed(mount);
            }
        });
        watch.mount_handlers.replace(vec![added, removed]);
        watch
    }

    // Watches the folders of the list, and no others; returns whether it
    // watched folders already and some of these are new, as files could
    // have come into them before they were watched
    pub(crate) fn set_folders(self: &Rc<Self>, list: WatchList) -> bool {
        self.available.replace(list.available);
        self.unavailable.set(list.unavailable);

        let mut monitors = self.monitors.borrow_mut();
        let watching = !monitors.is_empty();
        let wanted: HashSet<&PathBuf> = list.folders.iter().collect();
        monitors.retain(|path, monitor| {
            let keep = wanted.contains(path);
            if !keep {
                monitor.cancel();
            }
            keep
        });

        let mut added = false;
        for folder in list.folders {
            if monitors.contains_key(&folder) {
                continue;
            }
            match gio::File::for_path(&folder).monitor_directory(
                gio::FileMonitorFlags::WATCH_MOVES,
                None::<&gio::Cancellable>,
            ) {
                Ok(monitor) => {
                    let weak = Rc::downgrade(self);
                    monitor.connect_changed(move |_, file, other_file, event| {
                        let other_path = other_file.and_then(gio::File::path);
                        if changes_library(event, file.path().as_deref(), other_path.as_deref())
                            && let Some(watch) = weak.upgrade()
                        {
                            watch.schedule();
                        }
                    });
                    monitors.insert(folder, monitor);
                    added = true;
                }
                Err(e) => warn!("Unable to watch {folder:?}: {e}"),
            }
        }
        debug!("Watching {} folders", monitors.len());
        watching && added
    }

    fn mount_changed(self: &Rc<Self>, mount: &gio::Mount) {
        let Some(root) = mount.root().path() else {
            return;
        };
        if mount_changes_library(&root, &self.available.borrow(), self.unavailable.get()) {
            self.schedule();
        } else {
            debug!("Not reading the music folders for the drive at {root:?}");
        }
    }

    // Waits for the changes to stop
    fn schedule(self: &Rc<Self>) {
        if let Some(source) = self.pending.take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(SETTLE_TIME, move || {
            if let Some(watch) = weak.upgrade() {
                // It ran, so it's gone
                watch.pending.take();
                (watch.changed)();
            }
        });
        self.pending.replace(Some(source));
    }
}

// Whether a change in a watched folder can change the library: not one to
// the permissions or the times of a file alone, nor one to a hidden file,
// which the scan doesn't read, like a file manager's `.directory`, or a
// file synced under a hidden name until it's whole. A file renamed from a
// hidden name to another, as such a file is once it's whole, does change
// it. Nor does a playlist, which the scan doesn't read either, so that a
// playlist exported into a music folder, as phones and servers read them
// there, isn't followed by a scan. Files are local here, so they have paths
fn changes_library(
    event: gio::FileMonitorEvent,
    file: Option<&Path>,
    other_file: Option<&Path>,
) -> bool {
    let read = |path: &Path| is_library_entry(path) && !is_playlist_file(path);
    event != gio::FileMonitorEvent::AttributeChanged
        && (file.is_none_or(read) || other_file.is_some_and(read))
}

// Whether a file is an M3U playlist, or the temporary file GLib writes one
// to first, named like "Mix.m3u8.A1B2C3", which it then renames
fn is_playlist_file(path: &Path) -> bool {
    let is_playlist = |name: &Path| {
        name.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u") || ext.eq_ignore_ascii_case("m3u8"))
    };
    if is_playlist(path) {
        return true;
    }
    let temporary = path.extension().is_some_and(|ext| {
        ext.len() == 6 && ext.as_encoded_bytes().iter().all(u8::is_ascii_alphanumeric)
    });
    temporary
        && path
            .file_stem()
            .is_some_and(|stem| is_playlist(Path::new(stem)))
}

// Whether a drive mounted or unmounted at `root` can change the library:
// a music folder that's there is on it, or it's in one. While a music
// folder isn't there, any drive can be the one that brings it back
fn mount_changes_library(root: &Path, available: &[PathBuf], unavailable: bool) -> bool {
    unavailable
        || available
            .iter()
            .any(|folder| folder.starts_with(root) || root.starts_with(folder))
}

impl std::fmt::Debug for FolderWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderWatch")
            .field("folders", &self.monitors.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Drop for FolderWatch {
    fn drop(&mut self) {
        for monitor in self.monitors.get_mut().values() {
            monitor.cancel();
        }
        if let Some(source) = self.pending.get_mut().take() {
            source.remove();
        }
        for handler in self.mount_handlers.get_mut().drain(..) {
            self.volume_monitor.disconnect(handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_files_and_attributes_dont_change_the_library() {
        use gio::FileMonitorEvent::{AttributeChanged, Changed, Created, Deleted, Renamed};

        let track = Path::new("/music/Album/01.flac");
        let hidden = Path::new("/music/Album/.directory");
        assert!(changes_library(Created, Some(track), None));
        assert!(changes_library(Deleted, Some(track), None));
        assert!(!changes_library(AttributeChanged, Some(track), None));
        assert!(!changes_library(Changed, Some(hidden), None));
        assert!(!changes_library(
            Created,
            Some(Path::new("/music/.Trash-1000")),
            None
        ));

        // A file synced under a hidden name, then given its own
        let synced = Path::new("/music/Album/.syncthing.02.flac.tmp");
        let whole = Path::new("/music/Album/02.flac");
        assert!(!changes_library(Changed, Some(synced), None));
        assert!(changes_library(Renamed, Some(synced), Some(whole)));
        assert!(changes_library(Renamed, Some(whole), Some(synced)));

        // Without a path, it might
        assert!(changes_library(Changed, None, None));
    }

    #[test]
    fn playlist_files_dont_change_the_library() {
        use gio::FileMonitorEvent::{Created, Deleted, Renamed};

        let playlist = Path::new("/music/Playlists/Mix.m3u8");
        let temporary = Path::new("/music/Playlists/Mix.m3u8.A1b2C3");
        assert!(!changes_library(Created, Some(temporary), None));
        assert!(!changes_library(Renamed, Some(temporary), Some(playlist)));
        assert!(!changes_library(
            Deleted,
            Some(Path::new("/music/Old.M3U")),
            None
        ));

        // Covers, sheets and tracks still do, whatever their names
        for name in ["cover.jpg", "Album.cue", "Mix.m3u8.flac", "Mix.m3u8.bak"] {
            let path = Path::new("/music/Album").join(name);
            assert!(changes_library(Created, Some(&path), None), "{name}");
        }
        assert!(changes_library(
            Renamed,
            Some(temporary),
            Some(Path::new("/music/Album/01.flac"))
        ));
    }

    #[test]
    fn only_drives_with_music_folders_change_the_library() {
        let available = [
            PathBuf::from("/media/hdd/Music"),
            PathBuf::from("/home/a/Music"),
        ];
        // The drive a music folder is on
        assert!(mount_changes_library(
            Path::new("/media/hdd"),
            &available,
            false
        ));
        // A drive mounted in a music folder
        assert!(mount_changes_library(
            Path::new("/home/a/Music/usb"),
            &available,
            false
        ));
        // A phone, or a USB stick, elsewhere
        assert!(!mount_changes_library(
            Path::new("/run/media/a/PHONE"),
            &available,
            false
        ));
        assert!(!mount_changes_library(
            Path::new("/media/hdd2"),
            &available,
            false
        ));
        // A music folder that isn't there could be on any drive
        assert!(mount_changes_library(
            Path::new("/run/media/a/PHONE"),
            &available,
            true
        ));
    }
}
